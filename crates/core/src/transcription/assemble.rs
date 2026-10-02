//! Montagem PURA (sem banco, sem worker): bruto → blocos. Ordem em `assemble`: aplicar o offset do mic →
//! fusão de clusters → filtro de vazamento v2 (ANTES de atribuir/mesclar) → atribuição por palavra e corte
//! na troca de falante → mapeamento de rótulos contra a versão anterior → blocos.
use serde::Serialize;

use std::collections::{BTreeMap, BTreeSet, HashSet};

use super::params::{BleedParams, Params};
use super::protocol::WordTime;
use super::staging::{Energy, Segment, Track, Turn};
use crate::Result;
use crate::text::MAX_BLOCK_CHARS;

/// Blocos adjacentes do mesmo falante com intervalo menor que isto (s) se fundem.
pub const MERGE_GAP_S: f64 = 1.0;

/// Rótulos canônicos (a UI os traduz; no banco ficam sempre assim).
pub const LABEL_ME: &str = "Eu";
pub const LABEL_PERSON_PREFIX: &str = "Pessoa ";

pub fn person_label(n: usize) -> String {
    format!("{LABEL_PERSON_PREFIX}{n}")
}

/// Bloco da versão anterior usado só para mapear falantes (`label`, intervalo).
#[derive(Debug, Clone, PartialEq)]
pub struct PrevBlock {
    pub label: String,
    pub t_start: f64,
    pub t_end: f64,
}

pub struct AssembleInput<'a> {
    pub sys: &'a [Segment],
    pub mic: &'a [Segment],
    /// vazio = sem diarização (todo o sys vira "Pessoa 1")
    pub turns: &'a [Turn],
    pub sys_energy: Option<&'a Energy>,
    pub mic_energy: Option<&'a Energy>,
    /// `recording.json`: quanto o mic está adiantado/atrasado em relação ao sys (s); 0 se ausente.
    pub mic_offset_s: f64,
    pub params: &'a Params,
    /// blocos da versão ativa anterior (se houver), para preservar rótulos/nomes dos falantes
    pub previous: &'a [PrevBlock],
}

/// Bloco pronto para gravar (tempos no eixo do sys; `speaker` = rótulo canônico).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NewBlock {
    pub t_start: f64,
    pub t_end: f64,
    pub track: Track,
    pub speaker: String,
    pub text: String,
}

/// Segmento do mic descartado como vazamento (vira linha de `bleed_removals`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Removal {
    pub t_start: f64,
    pub t_end: f64,
    pub text: String,
    pub containment: Option<f64>,
    pub margin_db: Option<f64>,
    /// `text_and_energy` | `energy_short`
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Assembled {
    /// ordenados por `t_start`; `Eu` e `Pessoa N` intercalados
    pub blocks: Vec<NewBlock>,
    pub removals: Vec<Removal>,
    /// quantos clusters sobraram depois da fusão
    pub clusters: usize,
}

/// Montagem completa (ordem fixa do contrato §6): offset → fusão de clusters → filtro de vazamento →
/// atribuição por palavra → rótulos → blocos.
pub fn assemble(input: &AssembleInput) -> Result<Assembled> {
    let p = input.params;
    // 1. offset: o mic entra no eixo do sys
    let mic: Vec<Segment> = input.mic.iter().map(|s| shift(s, input.mic_offset_s)).collect();
    // 2. fusão de clusters
    let turns = if input.turns.is_empty() {
        Vec::new()
    } else {
        fuse_clusters(input.turns, p.min_cluster_pct, p.min_cluster_s)?
    };
    let clusters = turns.iter().map(|t| t.cluster).collect::<BTreeSet<_>>().len();
    // 3. vazamento (antes de atribuir/mesclar)
    let (mic, removals) = if p.bleed.enabled {
        drop_bleed(&mic, input.sys, input.mic_energy, input.sys_energy, input.mic_offset_s, &p.bleed)?
    } else {
        (mic, Vec::new())
    };
    // 4. atribuição por palavra
    let pieces = assign_speakers(input.sys, &turns)?;
    // 5. rótulos contra a versão anterior
    let spans: Vec<(f64, f64, i64)> = pieces.iter().map(|(a, b, c, _)| (*a, *b, *c)).collect();
    let labels: BTreeMap<i64, String> = map_labels(&spans, input.previous)?.into_iter().collect();
    // 6. ordenação (empate: sys antes do mic) e fusão de adjacentes
    let mut blocks: Vec<NewBlock> = pieces
        .into_iter()
        .map(|(a, b, c, text)| NewBlock { t_start: a, t_end: b, track: Track::Sys, speaker: labels[&c].clone(), text })
        .collect();
    blocks.extend(mic.iter().filter(|s| !s.text.trim().is_empty()).map(|s| NewBlock {
        t_start: s.start,
        t_end: s.end,
        track: Track::Mic,
        speaker: LABEL_ME.to_string(),
        text: s.text.trim().to_string(),
    }));
    blocks.sort_by(|a, b| {
        a.t_start.total_cmp(&b.t_start).then((a.track == Track::Mic).cmp(&(b.track == Track::Mic)))
    });
    Ok(Assembled { blocks: merge_adjacent(blocks), removals, clusters })
}

fn shift(s: &Segment, off: f64) -> Segment {
    let f = |t: f64| (t + off).max(0.0);
    Segment {
        start: f(s.start),
        end: f(s.end),
        text: s.text.clone(),
        words: s.words.iter().map(|w| WordTime(f(w.0), f(w.1), w.2.clone())).collect(),
    }
}

/// Funde blocos consecutivos (já ordenados) do mesmo falante com intervalo < `MERGE_GAP_S`.
fn merge_adjacent(blocks: Vec<NewBlock>) -> Vec<NewBlock> {
    let mut out: Vec<NewBlock> = Vec::with_capacity(blocks.len());
    for b in blocks {
        if let Some(last) = out.last_mut()
            && last.speaker == b.speaker
            && last.track == b.track
            && b.t_start - last.t_end < MERGE_GAP_S
            && last.text.chars().count() + 1 + b.text.chars().count() <= MAX_BLOCK_CHARS
        {
            last.t_end = last.t_end.max(b.t_end);
            last.text.push(' ');
            last.text.push_str(&b.text);
        } else {
            out.push(b);
        }
    }
    out
}

/// Palavras normalizadas: minúsculas, sem diacríticos nem pontuação.
fn tokens(s: &str) -> Vec<String> {
    deunicode::deunicode(s)
        .to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

/// Média (dB) do envelope em `[t0, t1]` (tempo do arquivo da trilha); `None` se o trecho não tem amostra.
fn mean_db(e: &Energy, t0: f64, t1: f64) -> Option<f64> {
    if e.step_ms == 0 || e.db.is_empty() {
        return None;
    }
    let step = f64::from(e.step_ms) / 1000.0;
    let a = ((t0.max(0.0) / step).floor() as usize).min(e.db.len());
    let b = ((t1.max(0.0) / step).ceil() as usize).clamp(a, e.db.len());
    let part = &e.db[a..b];
    (!part.is_empty()).then(|| part.iter().map(|v| f64::from(*v)).sum::<f64>() / part.len() as f64)
}

/// Filtro de vazamento v2. Remove um segmento do mic se (a) a CONTENÇÃO (fração das palavras dele presentes na
/// união do texto dos segmentos do sys sobrepostos, janela ± `tolerance_s`, com offset) ≥ `containment` E o
/// segmento tem ≥ `min_words` palavras, E o portão de energia fecha (RMS do mic < RMS do sys no mesmo trecho
/// − `margin_db`); segmentos com menos palavras só saem pelo portão de energia (`energy_short`). Sem
/// energia disponível NADA é removido. Devolve (mantidos, removidos).
///
/// `mic` já vem no eixo do sys (offset aplicado); `mic_offset_s` serve só para voltar ao tempo do arquivo do
/// mic ao ler o envelope de energia dele.
pub fn drop_bleed(
    mic: &[Segment],
    sys: &[Segment],
    mic_energy: Option<&Energy>,
    sys_energy: Option<&Energy>,
    mic_offset_s: f64,
    p: &BleedParams,
) -> Result<(Vec<Segment>, Vec<Removal>)> {
    let (Some(me), Some(se)) = (mic_energy, sys_energy) else {
        return Ok((mic.to_vec(), Vec::new()));
    };
    let mut kept = Vec::new();
    let mut removed = Vec::new();
    for m in mic {
        let toks = tokens(&m.text);
        let gate = match (mean_db(me, m.start - mic_offset_s, m.end - mic_offset_s), mean_db(se, m.start, m.end)) {
            (Some(a), Some(b)) => Some(a - b),
            _ => None,
        };
        let closed = gate.is_some_and(|margin| margin < -p.margin_db);
        if !closed || toks.is_empty() {
            kept.push(m.clone());
            continue;
        }
        let mut union: HashSet<String> = HashSet::new();
        for s in sys.iter().filter(|s| s.start < m.end + p.tolerance_s && s.end > m.start - p.tolerance_s) {
            union.extend(tokens(&s.text));
        }
        let containment = toks.iter().filter(|t| union.contains(*t)).count() as f64 / toks.len() as f64;
        let reason = if toks.len() >= p.min_words {
            (containment >= p.containment).then_some("text_and_energy")
        } else {
            Some("energy_short")
        };
        match reason {
            Some(reason) => removed.push(Removal {
                t_start: m.start,
                t_end: m.end,
                text: m.text.trim().to_string(),
                containment: Some(containment),
                margin_db: gate,
                reason: reason.to_string(),
            }),
            None => kept.push(m.clone()),
        }
    }
    Ok((kept, removed))
}

fn overlap(a0: f64, a1: f64, b0: f64, b1: f64) -> f64 {
    (a1.min(b1) - a0.max(b0)).max(0.0)
}

fn gap(a0: f64, a1: f64, b0: f64, b1: f64) -> f64 {
    (a0.max(b0) - a1.min(b1)).max(0.0)
}

/// Funde clusters com < `min_pct` % da fala total ou < `min_s` s de fala no cluster maior mais sobreposto/
/// próximo (por tempo). Devolve turnos com rótulos 0..n renumerados por tempo total de fala (maior = 0).
/// O maior cluster nunca é fundido (sempre sobra ao menos um).
pub fn fuse_clusters(turns: &[Turn], min_pct: f64, min_s: f64) -> Result<Vec<Turn>> {
    let mut speech: BTreeMap<i64, f64> = BTreeMap::new();
    for t in turns {
        *speech.entry(t.cluster).or_default() += (t.end - t.start).max(0.0);
    }
    let total: f64 = speech.values().sum();
    let biggest = speech.iter().max_by(|a, b| a.1.total_cmp(b.1).then(b.0.cmp(a.0))).map(|(c, _)| *c);
    let small = |c: i64| Some(c) != biggest && (speech[&c] < total * min_pct / 100.0 || speech[&c] < min_s);
    let large: Vec<i64> = speech.keys().copied().filter(|c| !small(*c)).collect();
    let mut map: BTreeMap<i64, i64> = speech.keys().map(|c| (*c, *c)).collect();
    for &c in speech.keys().filter(|c| small(**c)) {
        let mine: Vec<&Turn> = turns.iter().filter(|t| t.cluster == c).collect();
        // (sobreposição, -distância, fala): maior vence
        let best = large.iter().copied().max_by(|a, b| {
            let score = |l: i64| {
                let theirs = turns.iter().filter(|t| t.cluster == l);
                let (mut ov, mut dist) = (0.0, f64::MAX);
                for t in theirs {
                    for m in &mine {
                        ov += overlap(m.start, m.end, t.start, t.end);
                        dist = dist.min(gap(m.start, m.end, t.start, t.end));
                    }
                }
                (ov, dist, speech[&l])
            };
            let (sa, sb) = (score(*a), score(*b));
            sa.0.total_cmp(&sb.0).then(sb.1.total_cmp(&sa.1)).then(sa.2.total_cmp(&sb.2)).then(b.cmp(a))
        });
        if let Some(l) = best {
            map.insert(c, l);
        }
    }
    // renumera por fala total (maior = 0; empate: rótulo menor)
    let mut merged: BTreeMap<i64, f64> = BTreeMap::new();
    for (c, s) in &speech {
        *merged.entry(map[c]).or_default() += s;
    }
    let mut order: Vec<(i64, f64)> = merged.into_iter().collect();
    order.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    let rename: BTreeMap<i64, i64> = order.iter().enumerate().map(|(i, (c, _))| (*c, i as i64)).collect();
    Ok(turns.iter().map(|t| Turn { start: t.start, end: t.end, cluster: rename[&map[&t.cluster]] }).collect())
}

/// Cluster de maior sobreposição com `[a, b]` (sem sobreposição: o do turno mais próximo; sem turnos: 0).
fn cluster_at(turns: &[Turn], a: f64, b: f64) -> i64 {
    let mut per: BTreeMap<i64, f64> = BTreeMap::new();
    for t in turns {
        let o = overlap(a, b, t.start, t.end);
        if o > 0.0 {
            *per.entry(t.cluster).or_default() += o;
        }
    }
    if let Some((c, _)) = per.iter().max_by(|x, y| x.1.total_cmp(y.1).then(y.0.cmp(x.0))) {
        return *c;
    }
    turns
        .iter()
        .min_by(|x, y| gap(a, b, x.start, x.end).total_cmp(&gap(a, b, y.start, y.end)))
        .map_or(0, |t| t.cluster)
}

/// Atribui cada palavra do sys ao cluster por sobreposição e corta o segmento na troca; segmento sem palavras
/// vai inteiro ao cluster de maior sobreposição. Resultado: (inicio, fim, cluster, texto) em ordem.
pub fn assign_speakers(sys: &[Segment], turns: &[Turn]) -> Result<Vec<(f64, f64, i64, String)>> {
    let mut out = Vec::new();
    for seg in sys {
        let text = seg.text.trim();
        if text.is_empty() {
            continue;
        }
        if seg.words.is_empty() {
            out.push((seg.start, seg.end, cluster_at(turns, seg.start, seg.end), text.to_string()));
            continue;
        }
        let per_word: Vec<i64> = seg.words.iter().map(|w| cluster_at(turns, w.0, w.1)).collect();
        if per_word.iter().all(|c| *c == per_word[0]) {
            out.push((seg.start, seg.end, per_word[0], text.to_string()));
            continue;
        }
        let mut i = 0;
        while i < seg.words.len() {
            let mut j = i;
            while j + 1 < seg.words.len() && per_word[j + 1] == per_word[i] {
                j += 1;
            }
            let words: Vec<&str> = seg.words[i..=j].iter().map(|w| w.2.trim()).filter(|w| !w.is_empty()).collect();
            if !words.is_empty() {
                out.push((seg.words[i].0, seg.words[j].1, per_word[i], words.join(" ")));
            }
            i = j + 1;
        }
    }
    Ok(out)
}

/// `Some(N)` se o rótulo é `Pessoa N`.
fn person_number(label: &str) -> Option<usize> {
    label.strip_prefix(LABEL_PERSON_PREFIX)?.trim().parse().ok()
}

/// Mapeia cada cluster novo a um rótulo `Pessoa N`: guloso e 1-para-1 pela maior sobreposição de tempo com
/// os blocos `Pessoa N` da versão anterior (preserva nomes dados pelo usuário); clusters sem par ganham os
/// próximos números livres (nem usados agora, nem presentes na versão anterior). Devolve `(cluster, rótulo)`.
pub fn map_labels(blocks: &[(f64, f64, i64)], previous: &[PrevBlock]) -> Result<Vec<(i64, String)>> {
    let clusters: BTreeSet<i64> = blocks.iter().map(|b| b.2).collect();
    let prev: Vec<(usize, &PrevBlock)> = previous.iter().filter_map(|p| Some((person_number(&p.label)?, p))).collect();
    let mut pairs: BTreeMap<(i64, usize), f64> = BTreeMap::new();
    for b in blocks {
        for (n, p) in &prev {
            let o = overlap(b.0, b.1, p.t_start, p.t_end);
            if o > 0.0 {
                *pairs.entry((b.2, *n)).or_default() += o;
            }
        }
    }
    let mut ranked: Vec<((i64, usize), f64)> = pairs.into_iter().collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut assigned: BTreeMap<i64, usize> = BTreeMap::new();
    let mut used: BTreeSet<usize> = BTreeSet::new();
    for ((c, n), _) in ranked {
        if !assigned.contains_key(&c) && !used.contains(&n) {
            assigned.insert(c, n);
            used.insert(n);
        }
    }
    let taken: BTreeSet<usize> = prev.iter().map(|(n, _)| *n).chain(used.iter().copied()).collect();
    let mut next = 1;
    let mut out = Vec::new();
    for c in clusters {
        let n = match assigned.get(&c) {
            Some(n) => *n,
            None => {
                while taken.contains(&next) || assigned.values().any(|v| *v == next) {
                    next += 1;
                }
                next += 1;
                next - 1
            }
        };
        out.push((c, person_label(n)));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(a: f64, b: f64, text: &str) -> Segment {
        Segment { start: a, end: b, text: text.into(), words: vec![] }
    }

    fn words(a: f64, text: &str, step: f64) -> Segment {
        let ws: Vec<WordTime> =
            text.split(' ').enumerate().map(|(i, w)| WordTime(a + i as f64 * step, a + (i + 1) as f64 * step, format!(" {w}"))).collect();
        Segment { start: a, end: a + ws.len() as f64 * step, text: text.into(), words: ws }
    }

    fn flat(db: f32, secs: f64) -> Energy {
        Energy { step_ms: 100, db: vec![db; (secs * 10.0) as usize] }
    }

    fn turn(a: f64, b: f64, c: i64) -> Turn {
        Turn { start: a, end: b, cluster: c }
    }

    const BLEED: &str = "então o relatório de vendas ficou pronto ontem";

    fn bleed_params() -> BleedParams {
        BleedParams::default()
    }

    #[test]
    fn bleed_text_leak_is_removed_with_audit() {
        // mic repete o sys (com diacríticos e pontuação diferentes), mic 22 dB abaixo
        let sys = [seg(10.0, 14.0, BLEED)];
        let mic = [seg(10.2, 14.1, "Entao, o relatorio de vendas ficou pronto ontem!")];
        let (kept, removed) =
            drop_bleed(&mic, &sys, Some(&flat(-42.0, 20.0)), Some(&flat(-20.0, 20.0)), 0.0, &bleed_params()).unwrap();
        assert!(kept.is_empty());
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].reason, "text_and_energy");
        assert!(removed[0].containment.unwrap() >= 0.99);
        assert!((removed[0].margin_db.unwrap() + 22.0).abs() < 1e-6);
    }

    #[test]
    fn echo_in_sys_does_not_delete_real_speech_from_me() {
        // o sys tem ECO da fala do Eu (texto igual), mas o mic está 11 dB ACIMA do eco: portão aberto, preserva
        let sys = [seg(10.0, 14.0, BLEED)];
        let mic = [seg(10.0, 14.0, BLEED)];
        let (kept, removed) =
            drop_bleed(&mic, &sys, Some(&flat(-29.0, 20.0)), Some(&flat(-40.0, 20.0)), 0.0, &bleed_params()).unwrap();
        assert_eq!((kept.len(), removed.len()), (1, 0));
    }

    #[test]
    fn short_speech_only_goes_through_the_gate() {
        let sys = [seg(10.0, 12.0, "obrigado tchau")];
        let me = [seg(10.5, 11.0, "tá bom")];
        // 8 dB abaixo do sys (fala real do Eu com o outro falando junto): o portão de 15 dB preserva
        let (kept, removed) =
            drop_bleed(&me, &sys, Some(&flat(-29.0, 20.0)), Some(&flat(-21.0, 20.0)), 0.0, &bleed_params()).unwrap();
        assert_eq!((kept.len(), removed.len()), (1, 0));
        // com a margem antiga de 6 dB seria removida (o falso positivo do spike)
        let old = BleedParams { margin_db: 6.0, ..bleed_params() };
        let (_, removed) = drop_bleed(&me, &sys, Some(&flat(-29.0, 20.0)), Some(&flat(-21.0, 20.0)), 0.0, &old).unwrap();
        assert_eq!(removed.len(), 1);
        // 25 dB abaixo: portão fecha, curto sai só pela energia, mesmo com texto diferente
        let (kept, removed) =
            drop_bleed(&me, &sys, Some(&flat(-46.0, 20.0)), Some(&flat(-21.0, 20.0)), 0.0, &bleed_params()).unwrap();
        assert!(kept.is_empty());
        assert_eq!(removed[0].reason, "energy_short");
    }

    #[test]
    fn long_quiet_segment_with_different_text_is_kept() {
        let sys = [seg(10.0, 14.0, "bom dia a todos vamos começar")];
        let mic = [seg(10.0, 14.0, "pode ser sim por mim está ótimo")];
        let (kept, _) =
            drop_bleed(&mic, &sys, Some(&flat(-46.0, 20.0)), Some(&flat(-21.0, 20.0)), 0.0, &bleed_params()).unwrap();
        assert_eq!(kept.len(), 1);
    }

    #[test]
    fn without_energy_nothing_is_removed() {
        let sys = [seg(10.0, 14.0, BLEED)];
        let mic = [seg(10.0, 14.0, BLEED), seg(15.0, 15.5, "ok")];
        let (kept, removed) = drop_bleed(&mic, &sys, None, Some(&flat(-20.0, 20.0)), 0.0, &bleed_params()).unwrap();
        assert_eq!((kept.len(), removed.len()), (2, 0));
    }

    #[test]
    fn bleed_tolerance_window_and_offset_on_mic_energy() {
        // o mic começa 2 s depois do sys: o segmento do mic no eixo do sys é [12, 16]; a energia do mic
        // (tempo do arquivo) só é silenciosa em [10, 14] = eixo sys [12, 16]
        let sys = [seg(12.0, 16.0, BLEED)];
        let mic = [seg(12.0, 16.0, BLEED)];
        let mut me = flat(-20.0, 20.0); // alto fora da janela
        for v in me.db[100..140].iter_mut() {
            *v = -45.0;
        }
        let (_, removed) = drop_bleed(&mic, &sys, Some(&me), Some(&flat(-20.0, 20.0)), 2.0, &bleed_params()).unwrap();
        assert_eq!(removed.len(), 1);
        // sem informar o offset a janela [12, 16] pega metade alta do mic: média -32,5 dB (margem 12,5 < 15), preserva
        let (kept, _) = drop_bleed(&mic, &sys, Some(&me), Some(&flat(-20.0, 20.0)), 0.0, &bleed_params()).unwrap();
        assert_eq!(kept.len(), 1);
    }

    #[test]
    fn fuse_small_clusters_into_nearest_large_and_renumber() {
        // cluster 5: 60 s; cluster 2: 40 s; cluster 7: 3 s (<10 s) entre os dois, perto do 2
        let turns = [turn(0.0, 60.0, 5), turn(60.0, 98.0, 2), turn(98.0, 101.0, 7), turn(101.0, 103.0, 2)];
        let out = fuse_clusters(&turns, 5.0, 10.0).unwrap();
        let labels: Vec<i64> = out.iter().map(|t| t.cluster).collect();
        assert_eq!(labels, vec![0, 1, 1, 1]); // 5 -> 0 (maior), 2 e 7 -> 1
        // por percentual: 4 s de 100 s < 5 %, mesmo com min_s = 1
        let turns = [turn(0.0, 48.0, 0), turn(48.0, 96.0, 1), turn(96.0, 100.0, 2)];
        assert_eq!(fuse_clusters(&turns, 5.0, 1.0).unwrap().iter().map(|t| t.cluster).collect::<Vec<_>>(), vec![1, 0, 0]);
        // o maior cluster nunca é fundido (áudio curto: tudo < 10 s)
        let turns = [turn(0.0, 6.0, 3), turn(6.0, 9.0, 4)];
        assert_eq!(fuse_clusters(&turns, 5.0, 10.0).unwrap().iter().map(|t| t.cluster).collect::<Vec<_>>(), vec![0, 0]);
        assert!(fuse_clusters(&[], 5.0, 10.0).unwrap().is_empty());
    }

    #[test]
    fn segment_is_cut_where_the_speaker_changes() {
        let sys = [words(0.0, "um dois tres quatro cinco seis", 1.0)];
        let turns = [turn(0.0, 3.2, 0), turn(3.2, 6.0, 1)];
        let out = assign_speakers(&sys, &turns).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!((out[0].0, out[0].1, out[0].2, out[0].3.as_str()), (0.0, 3.0, 0, "um dois tres"));
        assert_eq!((out[1].0, out[1].1, out[1].2, out[1].3.as_str()), (3.0, 6.0, 1, "quatro cinco seis"));
        // a palavra "quatro" (3.0-4.0) fica com o cluster 1 (0,8 s) e não com o 0 (0,2 s)
    }

    #[test]
    fn segment_without_words_goes_whole_to_the_cluster_with_most_overlap_and_no_turns_is_person_one() {
        let sys = [seg(0.0, 10.0, "texto inteiro sem palavras")];
        let turns = [turn(0.0, 3.0, 0), turn(3.0, 10.0, 1)];
        assert_eq!(assign_speakers(&sys, &turns).unwrap()[0].2, 1);
        assert_eq!(assign_speakers(&sys, &[]).unwrap()[0].2, 0);
    }

    fn prev(label: &str, a: f64, b: f64) -> PrevBlock {
        PrevBlock { label: label.into(), t_start: a, t_end: b }
    }

    #[test]
    fn labels_are_mapped_one_to_one_by_overlap() {
        let previous = [prev("Pessoa 1", 0.0, 50.0), prev("Pessoa 2", 50.0, 100.0), prev("Outros", 0.0, 100.0)];
        // os clusters trocaram de número; o 0 novo cobre a fala que era da Pessoa 2
        let blocks = [(0.0, 48.0, 1), (50.0, 100.0, 0), (48.0, 50.0, 0)];
        let m = map_labels(&blocks, &previous).unwrap();
        assert_eq!(m, vec![(0, "Pessoa 2".to_string()), (1, "Pessoa 1".to_string())]);
        // um cluster a mais: o novo ganha o próximo número livre (3), nunca reaproveita rótulo já mapeado
        let blocks = [(0.0, 50.0, 0), (50.0, 90.0, 1), (90.0, 100.0, 2)];
        let m = map_labels(&blocks, &previous).unwrap();
        assert_eq!(m[0].1, "Pessoa 1");
        assert_eq!(m[1].1, "Pessoa 2");
        assert_eq!(m[2].1, "Pessoa 3");
        // dois clusters disputando a mesma pessoa anterior: só um leva; o outro recebe número novo
        let m = map_labels(&[(0.0, 40.0, 0), (40.0, 50.0, 1)], &previous).unwrap();
        assert_eq!(m, vec![(0, "Pessoa 1".to_string()), (1, "Pessoa 3".to_string())]);
        // sem versão anterior: 1, 2...
        let m = map_labels(&[(0.0, 5.0, 4), (5.0, 9.0, 9)], &[]).unwrap();
        assert_eq!(m, vec![(4, "Pessoa 1".to_string()), (9, "Pessoa 2".to_string())]);
    }

    fn params() -> Params {
        Params { threads: 2, ..Params::default() }
    }

    #[test]
    fn assemble_end_to_end_offset_order_merge_and_labels() {
        let sys = [seg(0.0, 4.5, "fala trecho 0"), seg(5.0, 9.5, "fala trecho 1"), seg(30.0, 34.0, "fala trecho 6")];
        let mic = [seg(9.0, 9.4, "eu trecho 1"), seg(10.0, 12.0, "eu trecho 2")]; // arquivo do mic
        let turns = [turn(0.0, 15.0, 0), turn(15.0, 40.0, 1)];
        let (me, se) = (flat(-25.0, 40.0), flat(-20.0, 40.0));
        let p = params();
        let out = assemble(&AssembleInput {
            sys: &sys,
            mic: &mic,
            turns: &turns,
            sys_energy: Some(&se),
            mic_energy: Some(&me),
            mic_offset_s: 0.5,
            params: &p,
            previous: &[prev("Pessoa 1", 0.0, 10.0), prev("Pessoa 2", 20.0, 40.0)],
        })
        .unwrap();
        assert_eq!(out.clusters, 2);
        assert!(out.removals.is_empty());
        let got: Vec<(f64, &str, &str)> = out.blocks.iter().map(|b| (b.t_start, b.speaker.as_str(), b.text.as_str())).collect();
        assert_eq!(
            got,
            vec![
                (0.0, "Pessoa 1", "fala trecho 0 fala trecho 1"), // gap 0,5 s: fundidos
                (9.5, "Eu", "eu trecho 1 eu trecho 2"),            // +0,5 s de offset; 10,5-9,9 < 1 s: fundidos
                (30.0, "Pessoa 2", "fala trecho 6"),
            ]
        );
        assert_eq!(out.blocks[1].track, Track::Mic);
        assert_eq!(out.blocks[1].t_end, 12.5);
    }

    #[test]
    fn interleaved_speakers_are_not_merged_across_each_other() {
        let sys = [seg(2.0, 3.0, "oi")];
        let mic = [seg(0.0, 1.8, "eu um"), seg(3.1, 4.0, "eu dois")];
        let p = Params { bleed: BleedParams { enabled: false, ..BleedParams::default() }, ..params() };
        let out = assemble(&AssembleInput { sys: &sys, mic: &mic, turns: &[], sys_energy: None, mic_energy: None, mic_offset_s: 0.0, params: &p, previous: &[] })
            .unwrap();
        let who: Vec<&str> = out.blocks.iter().map(|b| b.speaker.as_str()).collect();
        assert_eq!(who, vec!["Eu", "Pessoa 1", "Eu"]);
    }

    #[test]
    fn negative_offset_clamps_to_zero() {
        let s = shift(&seg(0.1, 2.0, "x"), -0.5);
        assert_eq!((s.start, s.end), (0.0, 1.5));
    }
}
