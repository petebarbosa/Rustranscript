//! Normalização de texto, slugs e consultas FTS.

pub const MAX_BLOCK_CHARS: usize = 5000;
pub const MAX_TITLE_CHARS: usize = 200;

/// Colapsa qualquer sequência de espaços em um espaço só e apara as pontas.
pub fn normalize_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn slugify(s: &str) -> String {
    let ascii = deunicode::deunicode(s).to_lowercase();
    let mut out = String::with_capacity(ascii.len());
    for c in ascii.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-');
    let mut out: String = out.chars().take(60).collect();
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// "revisao-de-sprint" → "Revisao de sprint"
pub fn humanize(slug: &str) -> String {
    let s = slug.replace('-', " ");
    let s = s.trim();
    let mut chars = s.chars();
    match chars.next() {
        Some(f) => f.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

pub fn word_count(s: &str) -> usize {
    s.split_whitespace().count()
}

/// Corta em até `max` caracteres sem quebrar palavra, com reticências.
pub fn preview(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max).collect();
    let cut = match cut.rfind(' ') {
        Some(i) => &cut[..i],
        None => &cut[..],
    };
    format!("{cut}…")
}

/// Texto do usuário → consulta FTS5 segura: cada palavra vira uma frase com prefixo
/// (`"relat"*`), todas obrigatórias. Aspas internas são escapadas. `None` se não sobrar nada.
pub fn fts_query(input: &str) -> Option<String> {
    let terms: Vec<String> = input
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{}\"*", t.replace('"', "\"\"")))
        .collect();
    if terms.is_empty() { None } else { Some(terms.join(" ")) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert_eq!(slugify("Relatório de Ação — Equipe!"), "relatorio-de-acao-equipe");
        assert_eq!(slugify("  ---  "), "");
        assert_eq!(humanize("spike-busca"), "Spike busca");
    }

    #[test]
    fn previews() {
        assert_eq!(preview("abc def ghi", 20), "abc def ghi");
        assert_eq!(preview("abc def ghi", 9), "abc def…");
    }

    #[test]
    fn fts() {
        assert_eq!(fts_query("relatório  x\"y").unwrap(), "\"relatório\"* \"x\"* \"y\"*");
        assert_eq!(fts_query(" -- "), None);
    }
}
