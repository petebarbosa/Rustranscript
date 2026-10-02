//! Glossário, parte pura (sem banco): motor de substituição, sugestão a partir de edições,
//! leitura de listas de termos e orçamento do prompt. O armazenamento fica em `rules.rs`.
use std::collections::HashSet;
use std::sync::OnceLock;

use regex::{Regex, RegexBuilder};

use crate::model::{Hit, RuleKind, Scope};
use crate::{Error, Result, text};

// ------------------------------------------------------------------ motor de substituição

/// Regra `replace` já pronta para o motor.
#[derive(Debug, Clone)]
pub struct ReplaceRule {
    pub scope: Option<Scope>,
    pub id: Option<i64>,
    pub pattern: String,
    pub replacement: String,
    pub case_sensitive: bool,
}

impl ReplaceRule {
    pub fn new(pattern: &str, replacement: &str, case_sensitive: bool) -> ReplaceRule {
        ReplaceRule { scope: None, id: None, pattern: pattern.into(), replacement: replacement.into(), case_sensitive }
    }
}

#[derive(Debug, Clone)]
pub struct Applied {
    pub text: String,
    /// Só as regras que agiram, na ordem em que foram dadas ao motor.
    pub hits: Vec<Hit>,
}

impl Applied {
    pub fn replacements(&self) -> usize {
        self.hits.iter().map(|h| h.count).sum()
    }
}

struct Compiled {
    rule: ReplaceRule,
    re: Regex,
    /// Onde a própria substituição aparece no texto; um casamento dentro dela já está resolvido
    /// (só existe quando o padrão casa dentro da substituição: "Service" → "Zenith Service").
    guard: Option<Regex>,
}

/// Caractere de palavra como o `\w` do crate `regex` (Unicode: letras, marcas, dígitos, `_`...).
fn is_word(c: char) -> bool {
    static WORD: OnceLock<Regex> = OnceLock::new();
    let mut buf = [0u8; 4];
    WORD.get_or_init(|| Regex::new(r"^\w$").unwrap()).is_match(c.encode_utf8(&mut buf))
}

/// O crate `regex` não tem look-around, e `\b` só separa `\w` de `\W`: num padrão que começa ou
/// termina em pontuação (`C#`, `.NET`) um `\b` nessa ponta nunca casaria. Por isso a fronteira
/// `\b` só entra do lado cuja ponta do padrão é caractere de palavra; do outro lado vale o
/// padrão como está. Espaços do padrão casam qualquer sequência de espaços.
fn build_regex(pattern: &str, case_sensitive: bool) -> Result<Regex> {
    let words: Vec<&str> = pattern.split_whitespace().collect();
    if words.is_empty() {
        return Err(Error::invalid("pattern is empty"));
    }
    let body = words.iter().map(|w| regex::escape(w)).collect::<Vec<_>>().join(r"\s+");
    let first = words[0].chars().next().unwrap();
    let last = words[words.len() - 1].chars().last().unwrap();
    let mut src = String::new();
    if is_word(first) {
        src.push_str(r"\b");
    }
    src.push_str(&body);
    if is_word(last) {
        src.push_str(r"\b");
    }
    RegexBuilder::new(&src)
        .case_insensitive(!case_sensitive)
        .build()
        .map_err(|e| Error::invalid(format!("pattern: {e}")))
}

pub struct Engine {
    rules: Vec<Compiled>,
}

struct Candidate {
    start: usize,
    end: usize,
    rule: usize,
    out: String,
}

impl Engine {
    pub fn new(rules: &[ReplaceRule]) -> Result<Engine> {
        let mut compiled = Vec::with_capacity(rules.len());
        for rule in rules {
            let re = build_regex(&rule.pattern, rule.case_sensitive)?;
            let guard = if re.is_match(&rule.replacement) { Some(build_regex(&rule.replacement, rule.case_sensitive)?) } else { None };
            compiled.push(Compiled { rule: rule.clone(), re, guard });
        }
        Ok(Engine { rules: compiled })
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Uma passada só sobre o texto original: a saída de uma troca nunca é reexaminada. Entre
    /// casamentos que se sobrepõem vence o mais longo (empate: o que começa antes, depois a
    /// ordem das regras, que põe as do cliente na frente das globais).
    pub fn apply(&self, input: &str) -> Applied {
        let mut cands: Vec<Candidate> = Vec::new();
        for (i, c) in self.rules.iter().enumerate() {
            let spans: Vec<(usize, usize)> =
                c.guard.as_ref().map(|g| g.find_iter(input).map(|m| (m.start(), m.end())).collect()).unwrap_or_default();
            for m in c.re.find_iter(input) {
                let (start, end) = (m.start(), m.end());
                if spans.iter().any(|&(gs, ge)| gs <= start && end <= ge) {
                    continue;
                }
                let out = adapt_case(m.as_str(), &c.rule.replacement, c.rule.case_sensitive);
                cands.push(Candidate { start, end, rule: i, out });
            }
        }
        cands.sort_by(|a, b| (b.end - b.start).cmp(&(a.end - a.start)).then(a.start.cmp(&b.start)).then(a.rule.cmp(&b.rule)));
        let mut chosen: Vec<Candidate> = Vec::new();
        for c in cands {
            if !chosen.iter().any(|x| c.start < x.end && x.start < c.end) {
                chosen.push(c);
            }
        }
        chosen.sort_by_key(|c| c.start);

        let mut text = String::with_capacity(input.len());
        let mut counts = vec![0usize; self.rules.len()];
        let mut at = 0;
        for c in &chosen {
            // casamento que já está certo (troca igual ao trecho): vence a disputa, mas não conta
            if c.out == input[c.start..c.end] {
                continue;
            }
            text.push_str(&input[at..c.start]);
            text.push_str(&c.out);
            at = c.end;
            counts[c.rule] += 1;
        }
        text.push_str(&input[at..]);
        let hits = self
            .rules
            .iter()
            .zip(counts)
            .filter(|(_, n)| *n > 0)
            .map(|(c, count)| Hit {
                scope: c.rule.scope,
                rule_id: c.rule.id,
                pattern: c.rule.pattern.clone(),
                replacement: c.rule.replacement.clone(),
                count,
            })
            .collect();
        Applied { text, hits }
    }
}

/// Atalho para um texto só.
pub fn apply_rules(text: &str, rules: &[ReplaceRule]) -> Result<Applied> {
    Ok(Engine::new(rules)?.apply(text))
}

/// Quantas vezes o padrão aparece no texto (mesmas regras de palavra/caixa do motor).
pub fn count_matches(text: &str, pattern: &str, case_sensitive: bool) -> usize {
    build_regex(pattern, case_sensitive).map(|re| re.find_iter(text).count()).unwrap_or(0)
}

/// Mantém a capitalização do trecho trocado (só quando a regra não diferencia caixa):
/// TUDO MAIÚSCULO (2+ letras) → substituição em maiúsculas; Primeira Maiúscula e substituição
/// começando em minúscula → capitaliza a primeira letra; senão, como foi escrita.
fn adapt_case(matched: &str, replacement: &str, case_sensitive: bool) -> String {
    if case_sensitive {
        return replacement.to_string();
    }
    let letters: Vec<char> = matched.chars().filter(|c| c.is_alphabetic()).collect();
    let cased = letters.iter().filter(|c| c.is_uppercase() || c.is_lowercase()).count();
    if cased >= 2 && letters.iter().all(|c| !c.is_lowercase()) {
        return replacement.to_uppercase();
    }
    let first_upper = matched.chars().next().is_some_and(char::is_uppercase);
    let mut chars = replacement.chars();
    match chars.next() {
        Some(f) if first_upper && f.is_lowercase() => f.to_uppercase().chain(chars).collect(),
        _ => replacement.to_string(),
    }
}

// ------------------------------------------------------------------ sugestão a partir de edições

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Suggestion {
    pub pattern: String,
    pub replacement: String,
}

/// Nº máximo de palavras de cada lado de uma sugestão.
const SUGGEST_MAX_WORDS: usize = 4;
/// Protege contra textos enormes: a tabela do diff cresce com o produto dos tamanhos.
const SUGGEST_MAX_CELLS: usize = 4_000_000;

fn is_edge_punct(c: char) -> bool {
    c.is_whitespace() || ".,;:!?…\"'“”‘’()[]{}«»¿¡—–-".contains(c)
}

/// Chave de comparação do diff: sem caixa e sem pontuação nas pontas.
fn token_key(tok: &str) -> String {
    let k = tok.trim_matches(is_edge_punct).to_lowercase();
    if k.is_empty() { tok.to_lowercase() } else { k }
}

/// Diff por palavras entre o texto antigo e o novo. Cada trecho contíguo alterado, com 1 a 4
/// palavras dos dois lados e diferença além de caixa/pontuação, vira uma sugestão
/// `pattern → replacement` (pontuação nas pontas removida). Inserções e remoções puras ficam de
/// fora. Só o trecho que mudou entra: "Zenit Service" → "Zenith Service" sugere
/// `Zenit → Zenith`, que também vale para outras frases.
pub fn suggest_from_edit(old: &str, new: &str) -> Vec<Suggestion> {
    let a: Vec<&str> = old.split_whitespace().collect();
    let b: Vec<&str> = new.split_whitespace().collect();
    let (ka, kb): (Vec<String>, Vec<String>) = (a.iter().map(|t| token_key(t)).collect(), b.iter().map(|t| token_key(t)).collect());
    let mut pre = 0;
    while pre < a.len() && pre < b.len() && ka[pre] == kb[pre] {
        pre += 1;
    }
    let mut suf = 0;
    while suf < a.len() - pre && suf < b.len() - pre && ka[a.len() - 1 - suf] == kb[b.len() - 1 - suf] {
        suf += 1;
    }
    let (ma, mb) = (&a[pre..a.len() - suf], &b[pre..b.len() - suf]);
    let (mka, mkb) = (&ka[pre..a.len() - suf], &kb[pre..b.len() - suf]);
    if ma.is_empty() && mb.is_empty() || (ma.len() + 1) * (mb.len() + 1) > SUGGEST_MAX_CELLS {
        return vec![];
    }

    // LCS pelo sufixo: dp[i][j] = tamanho da subsequência comum de ma[i..] e mb[j..]
    let (n, m) = (ma.len(), mb.len());
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if mka[i] == mkb[j] { dp[i + 1][j + 1] + 1 } else { dp[i + 1][j].max(dp[i][j + 1]) };
        }
    }
    let mut out: Vec<Suggestion> = Vec::new();
    let (mut i, mut j) = (0, 0);
    let (mut del, mut ins): (Vec<&str>, Vec<&str>) = (vec![], vec![]);
    let mut flush = |del: &mut Vec<&str>, ins: &mut Vec<&str>| {
        if (1..=SUGGEST_MAX_WORDS).contains(&del.len())
            && (1..=SUGGEST_MAX_WORDS).contains(&ins.len())
            && let (p, r) = (del.join(" "), ins.join(" "))
            && let (p, r) = (p.trim_matches(is_edge_punct).to_string(), r.trim_matches(is_edge_punct).to_string())
            && !p.is_empty()
            && !r.is_empty()
            && p != r
            && token_key(&p) != token_key(&r)
        {
            let s = Suggestion { pattern: p, replacement: r };
            if !out.contains(&s) {
                out.push(s);
            }
        }
        del.clear();
        ins.clear();
    };
    while i < n || j < m {
        if i < n && j < m && mka[i] == mkb[j] {
            flush(&mut del, &mut ins);
            i += 1;
            j += 1;
        } else if j >= m || (i < n && dp[i + 1][j] >= dp[i][j + 1]) {
            del.push(ma[i]);
            i += 1;
        } else {
            ins.push(mb[j]);
            j += 1;
        }
    }
    flush(&mut del, &mut ins);
    out
}

// ------------------------------------------------------------------ termos para o prompt

/// Orçamento do prompt/hotwords do faster-whisper (contexto anterior, limite de ~224 tokens).
pub const PROMPT_TOKEN_BUDGET: usize = 224;

/// Estimativa de tokens sem o tokenizador do modelo: ≈ 1 token a cada 3 caracteres (português
/// e nomes próprios tokenizam em pedaços curtos; é deliberadamente conservadora).
pub fn estimate_tokens(s: &str) -> usize {
    s.chars().count().div_ceil(3)
}

/// Chave de comparação de padrões/termos: espaços normalizados e sem diferenciar caixa.
pub fn norm_key(s: &str) -> String {
    text::normalize_ws(s).to_lowercase()
}

/// Escolhe os termos que cabem no orçamento, na ordem dada (a prioridade é de quem vem
/// primeiro), sem repetir (sem diferenciar caixa). Cada termo custa `estimate_tokens` + 1 do
/// separador; um termo que estoura o que sobra é pulado e os seguintes ainda podem entrar.
pub fn select_prompt_terms(ordered: &[String], budget: usize) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut used = 0;
    let mut out = Vec::new();
    for t in ordered {
        let t = text::normalize_ws(t);
        if t.is_empty() || !seen.insert(norm_key(&t)) {
            continue;
        }
        let cost = estimate_tokens(&t) + 1;
        if used + cost > budget {
            continue;
        }
        used += cost;
        out.push(t);
    }
    out
}

// ------------------------------------------------------------------ lista de termos em arquivo

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListLine {
    /// Número da linha (a partir de 1).
    pub line: usize,
    pub pattern: String,
    /// `Some` = regra `replace` (`errado -> certo`); `None` = termo.
    pub replacement: Option<String>,
    /// Linha com seta mas um dos lados vazio.
    pub invalid: bool,
}

/// Uma entrada por linha; linhas vazias e as que começam com `#` (comentário) são ignoradas —
/// `#` no meio da linha faz parte do termo ("C#"). `errado -> certo` (também `→` e `=>`) é
/// regra de troca; o resto é termo.
pub fn parse_list(content: &str) -> Vec<ListLine> {
    let content = content.trim_start_matches('\u{feff}');
    let mut out = Vec::new();
    for (i, raw) in content.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let arrow = ["->", "→", "=>"].iter().filter_map(|a| line.find(a).map(|p| (p, a.len()))).min();
        match arrow {
            Some((p, len)) => {
                let (l, r) = (text::normalize_ws(&line[..p]), text::normalize_ws(&line[p + len..]));
                out.push(ListLine { line: i + 1, invalid: l.is_empty() || r.is_empty(), pattern: l, replacement: Some(r) });
            }
            None => out.push(ListLine { line: i + 1, pattern: text::normalize_ws(line), replacement: None, invalid: false }),
        }
    }
    out
}

/// Tipo de uma linha lida do arquivo.
pub fn line_kind(l: &ListLine) -> RuleKind {
    if l.replacement.is_some() { RuleKind::Replace } else { RuleKind::Term }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(text: &str, rules: &[(&str, &str)]) -> String {
        let rs: Vec<_> = rules.iter().map(|(p, r)| ReplaceRule::new(p, r, false)).collect();
        apply_rules(text, &rs).unwrap().text
    }

    #[test]
    fn word_boundaries_and_accents() {
        assert_eq!(run("o gate caiu; gateway ok; navegate", &[("gate", "portão")]), "o portão caiu; gateway ok; navegate");
        // acento é letra: "relatório" não casa dentro de "relatórios" nem de "xrelatório"
        assert_eq!(run("o relatório, os relatórios, xrelatório", &[("relatório", "documento")]), "o documento, os relatórios, xrelatório");
        // padrão com acento no começo/fim e texto em maiúsculas acentuadas
        assert_eq!(run("AÇÃO e ação", &[("ação", "ato")]), "ATO e ato");
        // marca combinante (NFD) também é parte da palavra
        let nfd = "relato\u{301}rio";
        assert_eq!(run(nfd, &[("relato", "x")]), nfd);
    }

    #[test]
    fn patterns_with_punctuation_and_spaces() {
        assert_eq!(run("usamos c# e .net hoje; abc# nao", &[("c#", "C Sharp")]), "usamos C Sharp e .net hoje; abc# nao");
        // termina em pontuação: fronteira só no começo
        assert_eq!(run("falou com a Sra. Maria e a xsra. Ana", &[("sra.", "Senhora")]), "falou com a Senhora Maria e a xsra. Ana");
        assert_eq!(run("use o .net e o dotnet", &[(".net", "dotnet")]), "use o dotnet e o dotnet");
        // espaços do padrão casam qualquer sequência de espaços; metacaracteres são literais
        assert_eq!(run("o Gate   Wei Service (v2) caiu", &[("gate wei service (v2)", "Gateway Service")]), "o Gateway Service caiu");
        assert_eq!(run("a+b a.b axb", &[("a.b", "ok")]), "a+b ok axb");
    }

    #[test]
    fn preserves_capitalization() {
        let rs = [("gate wei", "gateway")];
        assert_eq!(run("gate wei", &rs), "gateway");
        assert_eq!(run("Gate wei caiu", &rs), "Gateway caiu");
        assert_eq!(run("GATE WEI caiu", &rs), "GATEWAY caiu");
        // substituição que já começa em maiúscula vale como foi escrita
        assert_eq!(run("gate wei", &[("gate wei", "Gateway")]), "Gateway");
        assert_eq!(run("Gate wei", &[("gate wei", "Gateway")]), "Gateway");
        // uma letra só maiúscula não é "tudo maiúsculo"
        assert_eq!(run("A casa", &[("a", "uma")]), "Uma casa");
        // acentuadas
        assert_eq!(run("ÓTIMO", &[("ótimo", "bom")]), "BOM");
    }

    #[test]
    fn case_sensitive_rules_only_match_exact_case() {
        let r = [ReplaceRule::new("API", "interface", true)];
        let a = apply_rules("API api Api", &r).unwrap();
        assert_eq!(a.text, "interface api Api");
        // e a substituição não sofre adaptação de caixa
        let r = [ReplaceRule::new("Gate", "gateway", true)];
        assert_eq!(apply_rules("Gate", &r).unwrap().text, "gateway");
    }

    #[test]
    fn longest_pattern_wins_over_overlapping_shorter() {
        let text = "o Gate Wei Service caiu";
        assert_eq!(run(text, &[("wei", "W"), ("gate wei service", "Gateway Service")]), "o Gateway Service caiu");
        assert_eq!(run(text, &[("gate wei service", "Gateway Service"), ("wei", "W")]), "o Gateway Service caiu");
        // um casamento longo que falha na fronteira não rouba o lugar do curto válido
        assert_eq!(run("Wei Services", &[("wei service", "X"), ("wei", "Y")]), "Y Services");
        // sem sobreposição, as duas agem
        assert_eq!(run("gate e wei", &[("gate", "A"), ("wei", "B")]), "A e B");
    }

    #[test]
    fn single_pass_never_cascades() {
        // b→c não age sobre a saída de a→b
        assert_eq!(run("a b", &[("a", "b"), ("b", "c")]), "b c");
        assert_eq!(run("a", &[("a", "b"), ("b", "a")]), "b");
    }

    #[test]
    fn idempotent_when_replacement_contains_pattern() {
        // fronteira de palavra já impede "Gatewayway"
        let rs = [("Gate", "Gateway")];
        let once = run("o Gate caiu", &rs);
        assert_eq!(once, "o Gateway caiu");
        assert_eq!(run(&once, &rs), once);
        // a substituição contém o padrão como palavra inteira: sem guarda viraria "Zenith Zenith Service"
        let rs = [("Service", "Zenith Service")];
        let once = run("o Service caiu", &rs);
        assert_eq!(once, "o Zenith Service caiu");
        assert_eq!(run(&once, &rs), once);
        assert_eq!(run("zenith service", &rs), "zenith service", "sem diferenciar caixa, já está resolvido");
        // padrão no começo da substituição
        let rs = [("Gateway Service", "Gateway Service API")];
        let once = run("o Gateway Service caiu", &rs);
        assert_eq!(once, "o Gateway Service API caiu");
        assert_eq!(run(&once, &rs), once);
        // regra que não muda nada não conta como troca
        let a = apply_rules("API", &[ReplaceRule::new("api", "API", false)]).unwrap();
        assert_eq!((a.text.as_str(), a.replacements()), ("API", 0));
    }

    #[test]
    fn reports_hits_per_rule() {
        let rules = [ReplaceRule::new("gate", "gateway", false), ReplaceRule::new("nada", "x", false), ReplaceRule::new("wei", "W", false)];
        let a = apply_rules("gate wei gate", &rules).unwrap();
        assert_eq!(a.text, "gateway W gateway");
        assert_eq!(a.hits.iter().map(|h| (h.pattern.as_str(), h.count)).collect::<Vec<_>>(), [("gate", 2), ("wei", 1)]);
        assert_eq!(a.replacements(), 3);
    }

    #[test]
    fn empty_pattern_is_rejected() {
        assert!(Engine::new(&[ReplaceRule::new("   ", "x", false)]).is_err());
    }

    #[test]
    fn counts_matches_like_the_engine() {
        assert_eq!(count_matches("Gate gate gateway", "gate", false), 2);
        assert_eq!(count_matches("Gate gate", "gate", true), 1);
    }

    fn sugg(old: &str, new: &str) -> Vec<(String, String)> {
        suggest_from_edit(old, new).into_iter().map(|s| (s.pattern, s.replacement)).collect()
    }

    #[test]
    fn suggests_changed_spans() {
        assert_eq!(sugg("o Zenit Service caiu", "o Zenith Service caiu"), [("Zenit".into(), "Zenith".into())]);
        assert_eq!(sugg("o Gate Wei Service caiu", "o Gateway Service caiu"), [("Gate Wei".into(), "Gateway".into())]);
        // vários trechos separados
        assert_eq!(
            sugg("Gate Wei e Zenit Service", "Gateway e Zenith Service"),
            [("Gate Wei".into(), "Gateway".into()), ("Zenit".into(), "Zenith".into())]
        );
        // pontuação nas pontas sai do padrão e da troca
        assert_eq!(sugg("Falou com Zenit, ontem.", "Falou com Zenith, ontem."), [("Zenit".into(), "Zenith".into())]);
        assert_eq!(sugg("é o (gate wei).", "é o (gateway)."), [("gate wei".into(), "gateway".into())]);
    }

    #[test]
    fn ignores_case_punctuation_insertions_and_rewrites() {
        assert!(sugg("o gateway caiu", "O Gateway caiu.").is_empty(), "só caixa/pontuação");
        assert!(sugg("o gateway caiu", "o gateway caiu de novo").is_empty(), "inserção pura");
        assert!(sugg("o gateway caiu de novo", "o gateway caiu").is_empty(), "remoção pura");
        assert!(sugg("a b c d e f", "x y z w v u").is_empty(), "mais de 4 palavras");
        assert!(sugg("igual", "igual").is_empty());
        assert!(sugg("", "algo").is_empty());
        // até 4 palavras dos dois lados
        assert_eq!(sugg("um a b c d dois", "um x y z w dois").len(), 1);
        // espaçamento: "web hook" → "webhook" é correção válida
        assert_eq!(sugg("o web hook falhou", "o webhook falhou"), [("web hook".into(), "webhook".into())]);
    }

    #[test]
    fn prompt_terms_respect_order_dedup_and_budget() {
        let t: Vec<String> = ["Zenitron", "zenitron", "  Gateway  Service ", "", "Kubernetes"].iter().map(|s| s.to_string()).collect();
        assert_eq!(select_prompt_terms(&t, 224), ["Zenitron", "Gateway Service", "Kubernetes"]);
        // custo = ceil(chars/3) + 1: "Zenitron" (8) → 4; "Gateway Service" (15) → 6
        assert_eq!(estimate_tokens("Zenitron"), 3);
        assert_eq!(select_prompt_terms(&t, 4), ["Zenitron"]);
        // "Gateway Service" (6) estoura os 9, mas "Kubernetes" (4 + 1 = 5) ainda cabe depois dele
        assert_eq!(select_prompt_terms(&t, 9), ["Zenitron", "Kubernetes"]);
        assert_eq!(select_prompt_terms(&t, 10), ["Zenitron", "Gateway Service"]);
        // um termo grande demais é pulado, os menores seguintes ainda entram
        let t: Vec<String> = ["a".repeat(60), "ok".into()].to_vec();
        assert_eq!(select_prompt_terms(&t, 10), ["ok"]);
        // muitos termos: nunca passa do orçamento
        let many: Vec<String> = (0..500).map(|i| format!("termo{i}")).collect();
        let chosen = select_prompt_terms(&many, PROMPT_TOKEN_BUDGET);
        assert!(chosen.iter().map(|t| estimate_tokens(t) + 1).sum::<usize>() <= PROMPT_TOKEN_BUDGET);
        assert!(chosen.len() > 10 && chosen.len() < 500);
    }

    #[test]
    fn parses_list_files() {
        let src = "\u{feff}# comentário\n\nKubernetes\n  Gate Wei -> Gateway  \nzenit service → Zenith Service\nabc => abd\nC# e .NET\n -> sem lado\nfoo ->\n   # outro comentário\n";
        let l = parse_list(src);
        let view: Vec<_> = l.iter().map(|e| (e.line, e.pattern.as_str(), e.replacement.as_deref(), e.invalid)).collect();
        assert_eq!(
            view,
            [
                (3, "Kubernetes", None, false),
                (4, "Gate Wei", Some("Gateway"), false),
                (5, "zenit service", Some("Zenith Service"), false),
                (6, "abc", Some("abd"), false),
                (7, "C# e .NET", None, false),
                (8, "", Some("sem lado"), true),
                (9, "foo", Some(""), true),
            ]
        );
        assert_eq!(line_kind(&l[1]), RuleKind::Replace);
        assert_eq!(line_kind(&l[0]), RuleKind::Term);
    }
}
