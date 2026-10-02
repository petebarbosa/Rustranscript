//! Modelos baixados pelo Rust (ureq + sha256; arquivo `.part` + rename; retomável por `Range`).
//! URLs, tamanhos e sha256 vêm do spike e são a fonte única; nada é baixado sem conferir o sha256.
//! Layout: `<dados>/models/whisper/large-v3-turbo@0a363e91/…` e `<dados>/models/diarization/…`.
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use rusqlite::params;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{Error, Result, db, schema};

/// Marca de modelo completo (`MODEL.json` na pasta do modelo).
pub const MARKER: &str = "MODEL.json";

/// Arquivo de um modelo. `extract`: o baixado é um `.tar.bz2` e o que vale é este membro (com seu sha256).
#[derive(Debug)]
pub struct ModelFile {
    /// Relativo à pasta do modelo.
    pub dest: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub bytes: u64,
    pub extract: Option<Extract>,
}

#[derive(Debug)]
pub struct Extract {
    pub member: &'static str,
    pub sha256: &'static str,
}

#[derive(Debug)]
pub struct ModelSpec {
    /// `whisper` | `segmentation` | `embedding` (vai na UI e no comando `models_import_local`).
    pub id: &'static str,
    /// Relativo a `<dados>/models`.
    pub dir: &'static str,
    pub files: &'static [ModelFile],
}

pub const WHISPER: ModelSpec = ModelSpec {
    id: "whisper",
    dir: "whisper/large-v3-turbo@0a363e91",
    files: &[
        ModelFile { dest: "model.bin", url: concat!("https://huggingface.co/dropbox-dash/faster-whisper-large-v3-turbo/resolve/0a363e9161cbc7ed1431c9597a8ceaf0c4f78fcf", "/model.bin"), sha256: "e76620f83d5f5b69efd3d87e3dc180c1bd21df9fbebacfd4335e5e1efcc018da", bytes: 1_617_884_929, extract: None },
        ModelFile { dest: "config.json", url: concat!("https://huggingface.co/dropbox-dash/faster-whisper-large-v3-turbo/resolve/0a363e9161cbc7ed1431c9597a8ceaf0c4f78fcf", "/config.json"), sha256: "b0253ea6c0d3bea6b1e19e91a02acfd3b53f4467362efcb5a3e6b16c9b3a9b7e", bytes: 2_263, extract: None },
        ModelFile { dest: "preprocessor_config.json", url: concat!("https://huggingface.co/dropbox-dash/faster-whisper-large-v3-turbo/resolve/0a363e9161cbc7ed1431c9597a8ceaf0c4f78fcf", "/preprocessor_config.json"), sha256: "7ccc62c6f2765af1f3b46c00c9b5894426835a05021c8b9c01eecb6dfb542711", bytes: 340, extract: None },
        ModelFile { dest: "tokenizer.json", url: concat!("https://huggingface.co/dropbox-dash/faster-whisper-large-v3-turbo/resolve/0a363e9161cbc7ed1431c9597a8ceaf0c4f78fcf", "/tokenizer.json"), sha256: "297b13372ac43916285644fb9687add3cc62ee2a1adb60da3dc25cc94c1871fd", bytes: 2_710_337, extract: None },
        ModelFile { dest: "vocabulary.json", url: concat!("https://huggingface.co/dropbox-dash/faster-whisper-large-v3-turbo/resolve/0a363e9161cbc7ed1431c9597a8ceaf0c4f78fcf", "/vocabulary.json"), sha256: "c69260f2ab26d659b7c398f9a2b2b48ed0df16c3b47d7326782fd9cba71690c1", bytes: 1_068_114, extract: None },
    ],
};

pub const SEGMENTATION: ModelSpec = ModelSpec {
    id: "segmentation",
    dir: "diarization/pyannote-segmentation-3-0",
    files: &[ModelFile {
        dest: "model.onnx",
        url: concat!("https://github.com/k2-fsa/sherpa-onnx/releases/download", "/speaker-segmentation-models/sherpa-onnx-pyannote-segmentation-3-0.tar.bz2"),
        sha256: "24615ee884c897d9d2ba09bb4d30da6bb1b15e685065962db5b02e76e4996488",
        bytes: 6_958_444,
        extract: Some(Extract {
            member: "sherpa-onnx-pyannote-segmentation-3-0/model.onnx",
            sha256: "220ad67ca923bef2fa91f2390c786097bf305bceb5e261d4af67b38e938e1079",
        }),
    }],
};

pub const EMBEDDING: ModelSpec = ModelSpec {
    id: "embedding",
    dir: "diarization/campplus-zh-en-advanced",
    files: &[ModelFile {
        dest: "model.onnx",
        url: concat!("https://github.com/k2-fsa/sherpa-onnx/releases/download", "/speaker-recongition-models/3dspeaker_speech_campplus_sv_zh_en_16k-common_advanced.onnx"),
        sha256: "aa3cfc16963a10586a9393f5035d6d6b57e98d358b347f80c2a30bf4f00ceba2",
        bytes: 28_281_164,
        extract: None,
    }],
};

pub const ALL: [&ModelSpec; 3] = [&WHISPER, &SEGMENTATION, &EMBEDDING];

pub fn spec(id: &str) -> Result<&'static ModelSpec> {
    ALL.into_iter().find(|m| m.id == id).ok_or_else(|| Error::invalid(format!("unknown model {id}")))
}

pub fn models_root(data_dir: &Path) -> PathBuf {
    data_dir.join("models")
}

/// Pastas/arquivos que o worker recebe.
#[derive(Debug, Clone, Serialize)]
pub struct ModelPaths {
    pub whisper_dir: PathBuf,
    pub seg_model: PathBuf,
    pub emb_model: PathBuf,
}

fn model_dir(data_dir: &Path, spec: &ModelSpec) -> PathBuf {
    models_root(data_dir).join(spec.dir)
}

/// Completo = tem a marca e todos os arquivos (os arquivos só entram por rename depois do sha256).
fn installed(data_dir: &Path, spec: &ModelSpec) -> bool {
    let dir = model_dir(data_dir, spec);
    dir.join(MARKER).is_file() && spec.files.iter().all(|f| dir.join(f.dest).is_file())
}

fn is_local(data_dir: &Path, spec: &ModelSpec) -> bool {
    std::fs::read(model_dir(data_dir, spec).join(MARKER))
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .is_some_and(|v| v["local"] == true)
}

/// Caminhos sem conferir a instalação: só para o worker falso (`FAKE_WORKER_ENV`), que não lê os modelos.
pub fn model_paths_unchecked(data_dir: &Path) -> ModelPaths {
    ModelPaths {
        whisper_dir: model_dir(data_dir, &WHISPER),
        seg_model: model_dir(data_dir, &SEGMENTATION).join("model.onnx"),
        emb_model: model_dir(data_dir, &EMBEDDING).join("model.onnx"),
    }
}

/// `models_missing` se qualquer um dos três não estiver completo.
pub fn model_paths(data_dir: &Path) -> Result<ModelPaths> {
    let missing: Vec<&str> = ALL.iter().filter(|m| !installed(data_dir, m)).map(|m| m.id).collect();
    if !missing.is_empty() {
        return Err(Error::transcription("models_missing", missing.join(", ")));
    }
    Ok(ModelPaths {
        whisper_dir: model_dir(data_dir, &WHISPER),
        seg_model: model_dir(data_dir, &SEGMENTATION).join("model.onnx"),
        emb_model: model_dir(data_dir, &EMBEDDING).join("model.onnx"),
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelStatus {
    pub id: String,
    pub installed: bool,
    /// Total a baixar (soma dos `bytes`).
    pub bytes_total: u64,
    /// Já presente em disco (completo ou `.part`).
    pub bytes_done: u64,
    /// Instalado de um arquivo local (`models_import_local`) e não do download.
    pub local: bool,
}

pub fn status(data_dir: &Path) -> Result<Vec<ModelStatus>> {
    Ok(ALL
        .iter()
        .map(|m| {
            let total: u64 = m.files.iter().map(|f| f.bytes).sum();
            let ok = installed(data_dir, m);
            let dir = model_dir(data_dir, m);
            let done = if ok {
                total
            } else {
                m.files
                    .iter()
                    .map(|f| {
                        let size = |p: PathBuf| std::fs::metadata(p).map_or(0, |md| md.len());
                        let whole = size(dir.join(f.dest));
                        if whole > 0 { f.bytes } else { size(part_path(&dir.join(f.dest))).min(f.bytes) }
                    })
                    .sum()
            };
            ModelStatus { id: m.id.to_string(), installed: ok, bytes_total: total, bytes_done: done, local: ok && is_local(data_dir, m) }
        })
        .collect())
}

/// Progresso do download (vira o evento `transcription-setup`).
#[derive(Debug, Clone, Serialize)]
pub struct DownloadProgress {
    pub model: String,
    pub file: String,
    pub bytes_done: u64,
    pub bytes_total: u64,
}

fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    dest.with_file_name(name)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut f = File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(&h.finalize()))
}

/// Arquivo a baixar (versão com donos de `ModelFile`; os testes apontam para um servidor local).
pub(crate) struct Item {
    pub model: String,
    pub dest: PathBuf,
    pub url: String,
    pub sha256: String,
    pub bytes: u64,
    /// (membro do `.tar.bz2`/`.tar.gz`, sha256 do membro se houver; o do arquivo inteiro vale sempre)
    pub extract: Option<(String, Option<String>)>,
}

fn dl_err(detail: impl std::fmt::Display) -> Error {
    Error::transcription("download_failed", detail)
}

fn cancelled() -> Error {
    Error::transcription("setup_cancelled", "cancelled by user")
}

/// Baixa `item.url` em `part` (retoma de onde parou por `Range`). Termina com o arquivo completo e com o
/// tamanho certo em `part` (o sha256 é conferido por quem chama).
fn fetch(item: &Item, part: &Path, on_progress: &mut dyn FnMut(&DownloadProgress), cancel: &AtomicBool) -> Result<()> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_recv_response(Some(Duration::from_secs(60)))
        .timeout_recv_body(Some(Duration::from_secs(60)))
        .build()
        .into();
    let progress = |done: u64| DownloadProgress {
        model: item.model.clone(),
        file: item.dest.file_name().unwrap_or_default().to_string_lossy().into_owned(),
        bytes_done: done,
        bytes_total: item.bytes,
    };
    let mut last_err = String::new();
    for attempt in 0..3 {
        if cancel.load(Ordering::Relaxed) {
            return Err(cancelled());
        }
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(500 * attempt));
        }
        let mut have = std::fs::metadata(part).map_or(0, |m| m.len());
        if have > item.bytes {
            std::fs::remove_file(part)?;
            have = 0;
        }
        if have == item.bytes {
            return Ok(());
        }
        let mut req = agent.get(&item.url).header("Accept-Encoding", "identity");
        if have > 0 {
            req = req.header("Range", format!("bytes={have}-"));
        }
        let resp = match req.call() {
            Ok(r) => r,
            Err(e) => {
                last_err = e.to_string();
                continue;
            }
        };
        let status = resp.status().as_u16();
        let mut file = match status {
            206 => {
                // só vale se começa onde pedimos
                let from = resp
                    .headers()
                    .get("content-range")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.strip_prefix("bytes "))
                    .and_then(|v| v.split(['-', '/']).next())
                    .and_then(|v| v.parse::<u64>().ok());
                if from != Some(have) {
                    return Err(dl_err(format!("{}: unexpected Content-Range", item.url)));
                }
                OpenOptions::new().append(true).open(part)?
            }
            200 => {
                // o servidor ignorou o Range: recomeça do zero
                have = 0;
                File::create(part)?
            }
            416 => {
                // nada além de `have`: o .part pode estar completo com tamanho diferente do esperado
                std::fs::remove_file(part)?;
                last_err = "416 range not satisfiable".into();
                continue;
            }
            code => return Err(dl_err(format!("{}: HTTP {code}", item.url))),
        };
        file.seek(SeekFrom::End(0))?;
        let mut body = resp.into_body().into_reader();
        let mut buf = vec![0u8; 64 * 1024];
        let mut done = have;
        let mut last = Instant::now();
        on_progress(&progress(done));
        let outcome: std::io::Result<()> = loop {
            if cancel.load(Ordering::Relaxed) {
                file.flush()?;
                return Err(cancelled());
            }
            match body.read(&mut buf) {
                Ok(0) => break Ok(()),
                Ok(n) => {
                    file.write_all(&buf[..n])?;
                    done += n as u64;
                    if last.elapsed() >= Duration::from_millis(200) {
                        last = Instant::now();
                        on_progress(&progress(done));
                    }
                }
                Err(e) => break Err(e),
            }
        };
        file.flush()?;
        drop(file);
        on_progress(&progress(done));
        if let Err(e) = outcome {
            last_err = e.to_string();
            continue; // retoma por Range na próxima volta
        }
        let len = std::fs::metadata(part)?.len();
        if len == item.bytes {
            return Ok(());
        }
        if len > item.bytes {
            std::fs::remove_file(part)?;
            return Err(dl_err(format!("{}: server sent more than {} bytes", item.url, item.bytes)));
        }
        last_err = format!("connection closed at {len} of {} bytes", item.bytes);
    }
    Err(dl_err(format!("{}: {last_err}", item.url)))
}

/// Extrai `member` de um `.tar.bz2` ou `.tar.gz` (reconhecido pelos primeiros bytes) para `out`, devolvendo o
/// sha256 do que foi extraído.
fn extract_member(archive: &Path, member: &str, out: &Path) -> Result<String> {
    let mut magic = [0u8; 2];
    File::open(archive)?.read_exact(&mut magic)?;
    let file = File::open(archive)?;
    let reader: Box<dyn Read> =
        if magic == [0x1f, 0x8b] { Box::new(flate2::read::GzDecoder::new(file)) } else { Box::new(bzip2::read::BzDecoder::new(file)) };
    let mut tar = tar::Archive::new(reader);
    for entry in tar.entries()? {
        let mut entry = entry?;
        if entry.path()?.to_string_lossy() != member {
            continue;
        }
        let mut f = File::create(out)?;
        let mut h = Sha256::new();
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = entry.read(&mut buf)?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
            f.write_all(&buf[..n])?;
        }
        f.flush()?;
        return Ok(hex(&h.finalize()));
    }
    Err(Error::transcription("checksum_mismatch", format!("{member}: not found in the archive")))
}

/// Baixa, confere e coloca `item.dest` no lugar. Erro de sha256 apaga o arquivo ruim.
pub(crate) fn install_item(item: &Item, on_progress: &mut dyn FnMut(&DownloadProgress), cancel: &AtomicBool) -> Result<()> {
    if let Some(parent) = item.dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let part = part_path(&item.dest);
    fetch(item, &part, on_progress, cancel)?;
    let got = sha256_file(&part)?;
    if got != item.sha256 {
        let _ = std::fs::remove_file(&part);
        return Err(Error::transcription("checksum_mismatch", format!("{}: sha256 {got}", item.url)));
    }
    match &item.extract {
        None => std::fs::rename(&part, &item.dest)?,
        Some((member, member_sha)) => {
            let tmp = part_path(&part); // `<dest>.part.part`
            let got = extract_member(&part, member, &tmp)?;
            if member_sha.as_ref().is_some_and(|want| *want != got) {
                let _ = std::fs::remove_file(&tmp);
                let _ = std::fs::remove_file(&part);
                return Err(Error::transcription("checksum_mismatch", format!("{member}: sha256 {got}")));
            }
            std::fs::rename(&tmp, &item.dest)?;
            let _ = std::fs::remove_file(&part);
        }
    }
    Ok(())
}

fn items_of(data_dir: &Path, spec: &ModelSpec) -> Vec<Item> {
    let dir = model_dir(data_dir, spec);
    spec.files
        .iter()
        .map(|f| Item {
            model: spec.id.to_string(),
            dest: dir.join(f.dest),
            url: f.url.to_string(),
            sha256: f.sha256.to_string(),
            bytes: f.bytes,
            extract: f.extract.as_ref().map(|e| (e.member.to_string(), Some(e.sha256.to_string()))),
        })
        .collect()
}

/// `MODEL.json` + linha em `app.db.models` (informativa; falha ao gravar a linha não invalida o modelo).
fn finish_model(data_dir: &Path, spec: &ModelSpec, local: bool) -> Result<()> {
    let dir = model_dir(data_dir, spec);
    let files: serde_json::Map<String, serde_json::Value> =
        spec.files.iter().map(|f| (f.dest.to_string(), serde_json::json!({ "sha256": f.sha256, "bytes": f.bytes, "url": f.url }))).collect();
    let marker = serde_json::json!({ "id": spec.id, "local": local, "files": files, "installed_at": db::now() });
    let tmp = dir.join(format!("{MARKER}.part"));
    std::fs::write(&tmp, serde_json::to_vec_pretty(&marker)?)?;
    std::fs::rename(&tmp, dir.join(MARKER))?;
    if let Ok(conn) = db::open(&data_dir.join("app.db"), schema::APP_MIGRATIONS) {
        let size: u64 = spec.files.iter().map(|f| f.bytes).sum();
        let engine = if spec.id == "whisper" { "faster-whisper" } else { "sherpa-onnx" };
        let _ = conn.execute("DELETE FROM models WHERE engine = ?1 AND name = ?2", params![engine, spec.id]);
        let _ = conn.execute(
            "INSERT INTO models (engine, name, path, size, installed_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![engine, spec.id, spec.dir, size as i64, db::now()],
        );
    }
    Ok(())
}

/// Baixa o que falta de `ids` (vazio = todos), retomando `.part` por `Range`, conferindo sha256 (e o do
/// membro extraído) antes do rename. `cancel` verdadeiro = para e deixa o `.part`. Erros: `download_failed`,
/// `checksum_mismatch` (apaga o arquivo ruim), `setup_cancelled`.
pub fn ensure(
    data_dir: &Path,
    ids: &[&str],
    on_progress: &mut dyn FnMut(&DownloadProgress),
    cancel: &AtomicBool,
) -> Result<()> {
    let specs: Vec<&ModelSpec> = if ids.is_empty() { ALL.to_vec() } else { ids.iter().map(|id| spec(id)).collect::<Result<_>>()? };
    for spec in specs {
        ensure_items(data_dir, spec, &items_of(data_dir, spec), on_progress, cancel)?;
    }
    Ok(())
}

fn ensure_items(
    data_dir: &Path,
    spec: &ModelSpec,
    items: &[Item],
    on_progress: &mut dyn FnMut(&DownloadProgress),
    cancel: &AtomicBool,
) -> Result<()> {
    if installed(data_dir, spec) {
        return Ok(());
    }
    std::fs::create_dir_all(model_dir(data_dir, spec))?;
    for item in items.iter().filter(|i| !i.dest.is_file()) {
        install_item(item, on_progress, cancel)?;
    }
    finish_model(data_dir, spec, false)
}

/// Opção "arquivo local": copia `path` (arquivo, ou pasta no caso do whisper) para o layout, conferindo
/// só a estrutura (não o sha256) e marcando `MODEL.json` com `"local": true`.
pub fn import_local(data_dir: &Path, id: &str, path: &Path) -> Result<()> {
    let spec = spec(id)?;
    let dir = model_dir(data_dir, spec);
    let staging = dir.with_file_name(format!("{}.import", dir.file_name().unwrap_or_default().to_string_lossy()));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;
    let result = (|| -> Result<()> {
        if spec.files.len() == 1 {
            if !path.is_file() {
                return Err(Error::invalid(format!("{id}: expected a file: {}", path.display())));
            }
            std::fs::copy(path, staging.join(spec.files[0].dest))?;
        } else {
            if !path.is_dir() {
                return Err(Error::invalid(format!("{id}: expected a folder: {}", path.display())));
            }
            for f in spec.files {
                let src = path.join(f.dest);
                if !src.is_file() {
                    return Err(Error::invalid(format!("{id}: missing {} in {}", f.dest, path.display())));
                }
                std::fs::copy(src, staging.join(f.dest))?;
            }
        }
        Ok(())
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }
    let _ = std::fs::remove_dir_all(&dir);
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::rename(&staging, &dir)?;
    finish_model(data_dir, spec, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Copy, PartialEq)]
    enum Mode {
        /// 200 sem Range, 206 com Range.
        Normal,
        /// Sempre 200 com o corpo todo (ignora Range).
        IgnoreRange,
        /// Na primeira requisição manda só metade do corpo e fecha; depois, `Normal`.
        DropFirst,
        /// Com Range responde 416; sem Range, 200.
        Reject416,
    }

    /// Servidor HTTP local de um corpo só (nada vai à internet). `ranges` registra o início do Range de cada pedido.
    struct Server {
        url: String,
        ranges: Arc<Mutex<Vec<Option<u64>>>>,
    }

    fn serve(body: Vec<u8>, mode: Mode) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/file", listener.local_addr().unwrap());
        let ranges = Arc::new(Mutex::new(Vec::new()));
        let log = ranges.clone();
        std::thread::spawn(move || {
            for (n, stream) in listener.incoming().enumerate() {
                let Ok(mut stream) = stream else { return };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut range = None;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("range: bytes=") {
                        range = v.trim().trim_end_matches('-').parse::<u64>().ok();
                    }
                }
                log.lock().unwrap().push(range);
                let total = body.len() as u64;
                let (head, data): (String, &[u8]) = match (mode, range) {
                    (Mode::Reject416, Some(_)) => (format!("HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{total}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"), &[]),
                    (Mode::Normal | Mode::DropFirst, Some(from)) if from < total => (
                        format!("HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {from}-{}/{total}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", total - 1, total - from),
                        &body[from as usize..],
                    ),
                    _ => (format!("HTTP/1.1 200 OK\r\nContent-Length: {total}\r\nConnection: close\r\n\r\n"), &body[..]),
                };
                let _ = stream.write_all(head.as_bytes());
                let data = if mode == Mode::DropFirst && n == 0 { &data[..data.len() / 2] } else { data };
                let _ = stream.write_all(data);
            }
        });
        Server { url, ranges }
    }

    fn sample(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 7 % 251) as u8).collect()
    }

    fn sha(bytes: &[u8]) -> String {
        hex(&Sha256::digest(bytes))
    }

    fn item(dir: &Path, url: &str, body: &[u8]) -> Item {
        Item { model: "t".into(), dest: dir.join("m").join("model.bin"), url: url.into(), sha256: sha(body), bytes: body.len() as u64, extract: None }
    }

    fn install(it: &Item) -> Result<()> {
        install_item(it, &mut |_| {}, &AtomicBool::new(false))
    }

    #[test]
    fn download_from_scratch_checks_sha_and_renames() {
        let dir = tempfile::tempdir().unwrap();
        let body = sample(300_000);
        let srv = serve(body.clone(), Mode::Normal);
        let it = item(dir.path(), &srv.url, &body);
        install(&it).unwrap();
        assert_eq!(std::fs::read(&it.dest).unwrap(), body);
        assert!(!part_path(&it.dest).exists());
        assert_eq!(*srv.ranges.lock().unwrap(), vec![None]);
    }

    #[test]
    fn resumes_a_part_file_with_range() {
        let dir = tempfile::tempdir().unwrap();
        let body = sample(300_000);
        let srv = serve(body.clone(), Mode::Normal);
        let it = item(dir.path(), &srv.url, &body);
        std::fs::create_dir_all(it.dest.parent().unwrap()).unwrap();
        std::fs::write(part_path(&it.dest), &body[..100_000]).unwrap();
        install(&it).unwrap();
        assert_eq!(std::fs::read(&it.dest).unwrap(), body);
        assert_eq!(*srv.ranges.lock().unwrap(), vec![Some(100_000)], "only the missing tail was requested");
    }

    #[test]
    fn server_ignoring_range_restarts_from_zero() {
        let dir = tempfile::tempdir().unwrap();
        let body = sample(200_000);
        let srv = serve(body.clone(), Mode::IgnoreRange);
        let it = item(dir.path(), &srv.url, &body);
        std::fs::create_dir_all(it.dest.parent().unwrap()).unwrap();
        std::fs::write(part_path(&it.dest), &body[..50_000]).unwrap();
        install(&it).unwrap();
        assert_eq!(std::fs::read(&it.dest).unwrap(), body, "no duplicated prefix");
    }

    #[test]
    fn dropped_connection_is_resumed_by_range() {
        let dir = tempfile::tempdir().unwrap();
        let body = sample(400_000);
        let srv = serve(body.clone(), Mode::DropFirst);
        let it = item(dir.path(), &srv.url, &body);
        install(&it).unwrap();
        assert_eq!(std::fs::read(&it.dest).unwrap(), body);
        let r = srv.ranges.lock().unwrap().clone();
        assert_eq!(r.len(), 2);
        assert_eq!(r[0], None);
        assert!(r[1].is_some_and(|from| from > 0 && from < body.len() as u64));
    }

    #[test]
    fn range_not_satisfiable_discards_the_part_and_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let body = sample(100_000);
        let srv = serve(body.clone(), Mode::Reject416);
        let it = item(dir.path(), &srv.url, &body);
        std::fs::create_dir_all(it.dest.parent().unwrap()).unwrap();
        std::fs::write(part_path(&it.dest), &body[..10_000]).unwrap();
        install(&it).unwrap();
        assert_eq!(std::fs::read(&it.dest).unwrap(), body);
    }

    #[test]
    fn complete_part_needs_no_request() {
        let dir = tempfile::tempdir().unwrap();
        let body = sample(50_000);
        let it = item(dir.path(), "http://127.0.0.1:1/never", &body);
        std::fs::create_dir_all(it.dest.parent().unwrap()).unwrap();
        std::fs::write(part_path(&it.dest), &body).unwrap();
        install(&it).unwrap();
        assert_eq!(std::fs::read(&it.dest).unwrap(), body);
    }

    #[test]
    fn wrong_sha256_fails_and_removes_the_bad_file() {
        let dir = tempfile::tempdir().unwrap();
        let body = sample(80_000);
        let srv = serve(body.clone(), Mode::Normal);
        let mut it = item(dir.path(), &srv.url, &body);
        it.sha256 = sha(b"other");
        let e = install(&it).unwrap_err();
        assert_eq!(e.code(), "checksum_mismatch");
        assert!(!it.dest.exists() && !part_path(&it.dest).exists());
    }

    #[test]
    fn http_error_status_is_download_failed() {
        let dir = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/x", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for s in listener.incoming() {
                let mut s = s.unwrap();
                let mut buf = [0u8; 1024];
                let _ = s.read(&mut buf);
                let _ = s.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            }
        });
        let it = item(dir.path(), &url, b"abc");
        assert_eq!(install(&it).unwrap_err().code(), "download_failed");
    }

    #[test]
    fn extracts_and_checks_the_tar_bz2_member() {
        let dir = tempfile::tempdir().unwrap();
        let member = sample(60_000);
        let mut tar_bytes = Vec::new();
        {
            let mut b = tar::Builder::new(&mut tar_bytes);
            for (name, data) in [("pkg/other.txt", &b"zz"[..]), ("pkg/model.onnx", &member[..])] {
                let mut h = tar::Header::new_gnu();
                h.set_size(data.len() as u64);
                h.set_mode(0o644);
                h.set_cksum();
                b.append_data(&mut h, name, data).unwrap();
            }
            b.finish().unwrap();
        }
        let mut enc = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
        enc.write_all(&tar_bytes).unwrap();
        let archive = enc.finish().unwrap();
        let srv = serve(archive.clone(), Mode::Normal);
        let mut it = item(dir.path(), &srv.url, &archive);
        it.extract = Some(("pkg/model.onnx".into(), Some(sha(&member))));
        install(&it).unwrap();
        assert_eq!(std::fs::read(&it.dest).unwrap(), member);
        assert!(!part_path(&it.dest).exists());
        // sha256 do membro errado: nada fica no lugar
        let _ = std::fs::remove_file(&it.dest);
        it.extract = Some(("pkg/model.onnx".into(), Some(sha(b"no"))));
        assert_eq!(install(&it).unwrap_err().code(), "checksum_mismatch");
        assert!(!it.dest.exists());
        // membro que não existe no pacote
        it.extract = Some(("pkg/missing".into(), Some(sha(&member))));
        assert_eq!(install(&it).unwrap_err().code(), "checksum_mismatch");
    }

    #[test]
    fn extracts_a_member_from_a_tar_gz() {
        let dir = tempfile::tempdir().unwrap();
        let member = sample(20_000);
        let mut tar_bytes = Vec::new();
        {
            let mut b = tar::Builder::new(&mut tar_bytes);
            let mut h = tar::Header::new_gnu();
            h.set_size(member.len() as u64);
            h.set_mode(0o755);
            h.set_cksum();
            b.append_data(&mut h, "pkg/tool", &member[..]).unwrap();
            b.finish().unwrap();
        }
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        enc.write_all(&tar_bytes).unwrap();
        let archive = enc.finish().unwrap();
        let srv = serve(archive.clone(), Mode::Normal);
        let mut it = item(dir.path(), &srv.url, &archive);
        it.extract = Some(("pkg/tool".into(), None));
        install(&it).unwrap();
        assert_eq!(std::fs::read(&it.dest).unwrap(), member);
    }

    #[test]
    fn cancel_stops_and_keeps_the_part_for_the_next_resume() {
        let dir = tempfile::tempdir().unwrap();
        let body = sample(2_000_000);
        let srv = serve(body.clone(), Mode::Normal);
        let it = item(dir.path(), &srv.url, &body);
        std::fs::create_dir_all(it.dest.parent().unwrap()).unwrap();
        std::fs::write(part_path(&it.dest), &body[..100_000]).unwrap();
        let cancel = AtomicBool::new(false);
        let e = install_item(&it, &mut |_| cancel.store(true, Ordering::Relaxed), &cancel).unwrap_err();
        assert_eq!(e.code(), "setup_cancelled");
        assert!(!it.dest.exists());
        let kept = std::fs::metadata(part_path(&it.dest)).unwrap().len();
        assert!(kept >= 100_000 && kept < body.len() as u64);
        // retoma de onde parou
        install(&it).unwrap();
        assert_eq!(std::fs::read(&it.dest).unwrap(), body);
        assert_eq!(srv.ranges.lock().unwrap().last().copied().flatten(), Some(kept));
    }

    #[test]
    fn ensure_items_writes_the_marker_and_status_reports_it() {
        static FILES: [ModelFile; 1] = [ModelFile { dest: "model.bin", url: "", sha256: "", bytes: 0, extract: None }];
        static SPEC: ModelSpec = ModelSpec { id: "whisper", dir: "t/model", files: &FILES };
        let dir = tempfile::tempdir().unwrap();
        let body = sample(30_000);
        let srv = serve(body.clone(), Mode::Normal);
        let mut it = item(dir.path(), &srv.url, &body);
        it.dest = model_dir(dir.path(), &SPEC).join("model.bin");
        assert!(!installed(dir.path(), &SPEC));
        ensure_items(dir.path(), &SPEC, std::slice::from_ref(&it), &mut |_| {}, &AtomicBool::new(false)).unwrap();
        assert!(installed(dir.path(), &SPEC) && !is_local(dir.path(), &SPEC));
        // segunda chamada: nada a fazer, nem requisição
        let n = srv.ranges.lock().unwrap().len();
        ensure_items(dir.path(), &SPEC, std::slice::from_ref(&it), &mut |_| {}, &AtomicBool::new(false)).unwrap();
        assert_eq!(srv.ranges.lock().unwrap().len(), n);
        // sem modelos reais: o status geral continua dizendo que falta
        assert!(status(dir.path()).unwrap().iter().all(|s| !s.installed));
        assert_eq!(model_paths(dir.path()).unwrap_err().code(), "models_missing");
    }

    #[test]
    fn import_local_validates_structure_and_marks_local() {
        let dir = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        assert_eq!(import_local(dir.path(), "embedding", &src.path().join("nope.onnx")).unwrap_err().code(), "invalid");
        assert_eq!(import_local(dir.path(), "whisper", src.path()).unwrap_err().code(), "invalid");
        assert_eq!(import_local(dir.path(), "nope", src.path()).unwrap_err().code(), "invalid");
        let f = src.path().join("e.onnx");
        std::fs::write(&f, b"x").unwrap();
        import_local(dir.path(), "embedding", &f).unwrap();
        let st = status(dir.path()).unwrap();
        let emb = st.iter().find(|s| s.id == "embedding").unwrap();
        assert!(emb.installed && emb.local);
    }
}
