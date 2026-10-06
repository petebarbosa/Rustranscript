//! Pular cortes (issue #23). Os cortes são intervalos `[início, fim)` na linha do tempo ORIGINAL da chamada; o
//! player não os toca. Em vez de remendar o motor, o mixer e o WSOLA trabalham numa linha do tempo "comprimida"
//! (a chamada sem os cortes): para eles o áudio é contínuo, então velocidade e posição não têm o que quebrar
//! numa emenda. Só nas bordas da API (`Session::seek`, `Session::position`) a posição é convertida:
//! - chegar ao começo de um corte já é estar no fim dele (a posição pula de `S` para `E`);
//! - pular para dentro de um corte cai no fim dele.
//!
//! Tudo em amostras da taxa do mixer.

/// Cortes em amostras (fundidos, ordenados, dentro da chamada) e a conversão entre as duas linhas do tempo.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CutMap {
    /// `(início, fim)` na linha original, sem sobreposição nem encostando um no outro.
    spans: Vec<(u64, u64)>,
    removed: u64,
}

impl CutMap {
    /// `cuts_s` em segundos; vazios, fora da chamada (`len` amostras) e sobrepostos são ajustados.
    pub fn new(cuts_s: &[(f64, f64)], rate: u32, len: u64) -> CutMap {
        let to = |s: f64| (s.max(0.0) * f64::from(rate)).round() as u64;
        let mut v: Vec<(u64, u64)> =
            cuts_s.iter().filter(|(s, e)| s.is_finite() && e.is_finite()).map(|&(s, e)| (to(s).min(len), to(e).min(len))).filter(|(s, e)| e > s).collect();
        v.sort_unstable();
        let mut spans: Vec<(u64, u64)> = Vec::with_capacity(v.len());
        for (s, e) in v {
            match spans.last_mut() {
                Some(last) if s <= last.1 => last.1 = last.1.max(e),
                _ => spans.push((s, e)),
            }
        }
        let removed = spans.iter().map(|(s, e)| e - s).sum();
        CutMap { spans, removed }
    }

    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }

    /// Amostras que os cortes tiram da chamada.
    pub fn removed(&self) -> u64 {
        self.removed
    }

    /// Linha original → comprimida. Dentro de um corte: o ponto onde ele cai (o mesmo do fim dele).
    pub fn to_comp(&self, orig: u64) -> u64 {
        let mut gone = 0;
        for &(s, e) in &self.spans {
            if orig >= e {
                gone += e - s;
            } else if orig > s {
                return s - gone;
            } else {
                break;
            }
        }
        orig - gone
    }

    /// Comprimida → original. Um ponto que cai na emenda é o FIM do corte (já passou do começo dele).
    pub fn to_orig(&self, comp: u64) -> u64 {
        let mut gone = 0;
        for &(s, e) in &self.spans {
            if comp >= s - gone {
                gone += e - s;
            } else {
                break;
            }
        }
        comp + gone
    }

    /// Começo do primeiro corte depois de `orig` (onde a leitura contínua tem de parar).
    pub fn next_start(&self, orig: u64) -> Option<u64> {
        self.spans.iter().map(|&(s, _)| s).find(|&s| s > orig)
    }
}
