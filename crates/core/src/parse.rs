//! Leitura das transcrições `.txt` do pipeline antigo (`[HH:MM:SS] Falante: texto`).
//! `merge` reproduz exatamente o protótipo (`txt_to_html.py`), porque as edições salvas lá
//! são chaveadas pelo segundo inicial de cada bloco.
use std::sync::LazyLock;

use regex::Regex;

static LINE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[(\d+):(\d{2})(?::(\d{2}))?\]\s*([^:\]]+?):\s*(.*)$").unwrap());
static STEM_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(call_(\d{4}-\d{2}-\d{2})_(\d{2})-(\d{2})-(\d{2}))(?:_(.+?))?(?:_v(\d+))?$").unwrap()
});

pub const MERGE_GAP_S: u32 = 60;
pub const MERGE_CAP_CHARS: usize = 700;

#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub t: u32,
    pub speaker: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub t_start: u32,
    pub t_end: u32,
    pub speaker: String,
    pub text: String,
}

pub fn parse(text: &str) -> Vec<Segment> {
    let mut segs: Vec<Segment> = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(c) = LINE_RE.captures(line) {
            let n = |i: usize| c.get(i).map(|m| m.as_str().parse::<u32>().unwrap_or(0));
            let t = match n(3) {
                None => n(1).unwrap() * 60 + n(2).unwrap(),
                Some(s) => n(1).unwrap() * 3600 + n(2).unwrap() * 60 + s,
            };
            segs.push(Segment { t, speaker: c[4].trim().to_string(), text: c[5].trim().to_string() });
        } else if let Some(last) = segs.last_mut() {
            last.text.push(' ');
            last.text.push_str(line);
        }
    }
    segs
}

pub fn merge(segs: &[Segment], gap: u32, cap: usize) -> Vec<Block> {
    let mut blocks: Vec<Block> = Vec::new();
    for s in segs {
        if let Some(b) = blocks.last_mut()
            && b.speaker == s.speaker
            && s.t.saturating_sub(b.t_end) <= gap
            && b.text.chars().count() + s.text.chars().count() <= cap
        {
            b.text.push(' ');
            b.text.push_str(&s.text);
            b.t_end = s.t;
            continue;
        }
        blocks.push(Block { t_start: s.t, t_end: s.t, speaker: s.speaker.clone(), text: s.text.clone() });
    }
    blocks
}

#[derive(Debug, Clone, PartialEq)]
pub struct Stem {
    /// `call_YYYY-MM-DD_HH-MM-SS` — chave estável da chamada.
    pub key: String,
    pub date: String,
    pub time: String,
    pub slug: Option<String>,
    pub version: u32,
}

impl Stem {
    pub fn started_at(&self) -> String {
        format!("{}T{}", self.date, self.time)
    }
}

pub fn parse_stem(stem: &str) -> Option<Stem> {
    let c = STEM_RE.captures(stem)?;
    Some(Stem {
        key: c[1].to_string(),
        date: c[2].to_string(),
        time: format!("{}:{}:{}", &c[3], &c[4], &c[5]),
        slug: c.get(6).map(|m| m.as_str().to_string()),
        version: c.get(7).and_then(|m| m.as_str().parse().ok()).unwrap_or(1),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_both_timestamp_forms_and_continuations() {
        let segs = parse("[00:05] Speaker 1: oi\ncontinua aqui\n\n[01:02:03] Eu: tudo bem?\nlixo sem prefixo");
        assert_eq!(
            segs,
            vec![
                Segment { t: 5, speaker: "Speaker 1".into(), text: "oi continua aqui".into() },
                Segment { t: 3723, speaker: "Eu".into(), text: "tudo bem? lixo sem prefixo".into() },
            ]
        );
    }

    #[test]
    fn ignores_text_before_first_timestamp() {
        assert!(parse("cabeçalho solto\n").is_empty());
    }

    #[test]
    fn merges_like_the_prototype() {
        let segs = vec![
            Segment { t: 0, speaker: "Outros".into(), text: "a".into() },
            Segment { t: 30, speaker: "Outros".into(), text: "b".into() },
            Segment { t: 100, speaker: "Outros".into(), text: "c".into() }, // gap > 60
            Segment { t: 101, speaker: "Eu".into(), text: "d".into() },
        ];
        let b = merge(&segs, MERGE_GAP_S, MERGE_CAP_CHARS);
        assert_eq!(b.len(), 3);
        assert_eq!((b[0].t_start, b[0].t_end, b[0].text.as_str()), (0, 30, "a b"));
        assert_eq!(b[1].t_start, 100);
    }

    #[test]
    fn merge_cap_counts_characters_not_bytes() {
        let long = "ã".repeat(350);
        let segs = vec![
            Segment { t: 0, speaker: "Eu".into(), text: long.clone() },
            Segment { t: 1, speaker: "Eu".into(), text: long },
        ];
        assert_eq!(merge(&segs, 60, 700).len(), 1);
    }

    #[test]
    fn stems() {
        let s = parse_stem("call_2026-09-30_13-10-55_spike-busca_v2").unwrap();
        assert_eq!(s.key, "call_2026-09-30_13-10-55");
        assert_eq!(s.slug.as_deref(), Some("spike-busca"));
        assert_eq!(s.version, 2);
        assert_eq!(s.started_at(), "2026-09-30T13:10:55");
        let s = parse_stem("call_2026-10-01_11-44-47").unwrap();
        assert_eq!((s.slug, s.version), (None, 1));
        assert!(parse_stem("notes").is_none());
    }
}
