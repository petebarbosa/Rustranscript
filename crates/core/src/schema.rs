//! Migrações. Nunca editar uma migração já publicada: acrescentar uma nova ao fim da lista.

pub const APP_MIGRATIONS: &[&str] = &[r#"
CREATE TABLE libraries (
    id          INTEGER PRIMARY KEY,
    name        TEXT NOT NULL,
    kind        TEXT NOT NULL CHECK (kind IN ('inbox', 'company')),
    -- caminho absoluto da raiz; para a inbox fica vazio e é resolvido como <dados>/inbox
    path        TEXT NOT NULL,
    created_at  TEXT NOT NULL
);
CREATE UNIQUE INDEX libraries_one_inbox ON libraries(kind) WHERE kind = 'inbox';
CREATE UNIQUE INDEX libraries_path ON libraries(path) WHERE kind = 'company';

CREATE TABLE settings (
    key    TEXT PRIMARY KEY,
    value  TEXT NOT NULL
);

CREATE TABLE glossary_global (
    id              INTEGER PRIMARY KEY,
    kind            TEXT NOT NULL CHECK (kind IN ('term', 'replace')),
    pattern         TEXT NOT NULL,
    replacement     TEXT,
    case_sensitive  INTEGER NOT NULL DEFAULT 0,
    created_at      TEXT NOT NULL
);

CREATE TABLE models (
    id            INTEGER PRIMARY KEY,
    engine        TEXT NOT NULL,
    name          TEXT NOT NULL,
    path          TEXT NOT NULL,
    size          INTEGER,
    installed_at  TEXT NOT NULL
);
"#,
// 2 (fase 2): de onde veio a regra global criada a partir de uma edição. O id de
// `edit_history` só vale dentro de uma biblioteca, por isso vem junto o `source_library_id`.
r#"
ALTER TABLE glossary_global ADD COLUMN source_edit_id INTEGER;
ALTER TABLE glossary_global ADD COLUMN source_library_id INTEGER;
"#,
// 3 (fase 4): fila de transcrição (dona: a GUI). Uma tarefa por chamada em aberto (queued|running).
// `library_id`/`call_id` não têm FK (bancos diferentes); mover a chamada com tarefa aberta = `conflict`.
// `base_job_id` = tarefa cujo bruto (`tx_*` na biblioteca) alimenta `rediarize`/`resegment`.
r#"
CREATE TABLE transcription_jobs (
    id            INTEGER PRIMARY KEY,
    library_id    INTEGER NOT NULL,
    call_id       INTEGER NOT NULL,
    call_key      TEXT NOT NULL,
    kind          TEXT NOT NULL CHECK (kind IN ('full', 'rediarize', 'resegment')),
    state         TEXT NOT NULL CHECK (state IN ('queued', 'running', 'done', 'failed', 'cancelled')),
    options_json  TEXT NOT NULL DEFAULT '{}',
    base_job_id   INTEGER,
    attempts      INTEGER NOT NULL DEFAULT 0,
    stage         TEXT,
    progress      REAL,
    error_code    TEXT,
    error_detail  TEXT,
    created_at    TEXT NOT NULL,
    started_at    TEXT,
    finished_at   TEXT
);
CREATE INDEX transcription_jobs_open ON transcription_jobs(state, id) WHERE state IN ('queued', 'running');
CREATE UNIQUE INDEX transcription_jobs_one_open ON transcription_jobs(library_id, call_id)
    WHERE state IN ('queued', 'running');
"#];

pub const LIBRARY_MIGRATIONS: &[&str] = &[r#"
CREATE TABLE clients (
    id          INTEGER PRIMARY KEY,
    name        TEXT NOT NULL,
    slug        TEXT NOT NULL UNIQUE,
    created_at  TEXT NOT NULL
);

CREATE TABLE calls (
    id                 INTEGER PRIMARY KEY,
    key                TEXT NOT NULL UNIQUE,          -- call_YYYY-MM-DD_HH-MM-SS
    client_id          INTEGER REFERENCES clients(id) ON DELETE SET NULL,
    title              TEXT NOT NULL DEFAULT '',      -- vazio: a UI mostra "Chamada de <data>"
    slug               TEXT NOT NULL DEFAULT '',
    started_at         TEXT NOT NULL,
    duration_s         INTEGER NOT NULL DEFAULT 0,
    language           TEXT,
    expected_speakers  INTEGER,
    dir                TEXT,                          -- pasta da chamada, relativa à raiz da biblioteca
    mic_path           TEXT,                          -- relativos à raiz da biblioteca
    sys_path           TEXT,
    audio_deleted_at   TEXT,
    created_at         TEXT NOT NULL
);
CREATE INDEX calls_client ON calls(client_id, started_at);

CREATE TABLE import_sources (
    id       INTEGER PRIMARY KEY,
    call_id  INTEGER NOT NULL REFERENCES calls(id) ON DELETE CASCADE,
    kind     TEXT NOT NULL CHECK (kind IN ('transcript', 'mic', 'sys', 'edits', 'chapters')),
    path     TEXT NOT NULL,                           -- absoluto: arquivo original fora da biblioteca
    size     INTEGER,
    UNIQUE (call_id, path)
);

CREATE TABLE transcripts (
    id           INTEGER PRIMARY KEY,
    call_id      INTEGER NOT NULL REFERENCES calls(id) ON DELETE CASCADE,
    version      INTEGER NOT NULL,
    model        TEXT,
    engine       TEXT,
    params_json  TEXT,
    source_file  TEXT,
    created_at   TEXT NOT NULL,
    is_active    INTEGER NOT NULL DEFAULT 0,
    UNIQUE (call_id, version)
);
CREATE UNIQUE INDEX transcripts_one_active ON transcripts(call_id) WHERE is_active = 1;

CREATE TABLE speakers (
    id       INTEGER PRIMARY KEY,
    call_id  INTEGER NOT NULL REFERENCES calls(id) ON DELETE CASCADE,
    track    TEXT NOT NULL CHECK (track IN ('mic', 'sys')),
    label    TEXT NOT NULL,
    name     TEXT,
    UNIQUE (call_id, label)
);

CREATE TABLE blocks (
    id             INTEGER PRIMARY KEY,
    transcript_id  INTEGER NOT NULL REFERENCES transcripts(id) ON DELETE CASCADE,
    seq            INTEGER NOT NULL,
    t_start        REAL NOT NULL,
    t_end          REAL NOT NULL,
    speaker_id     INTEGER NOT NULL REFERENCES speakers(id),
    original_text  TEXT NOT NULL,
    text           TEXT NOT NULL,
    edited_at      TEXT,
    UNIQUE (transcript_id, seq)
);

CREATE TABLE chapters (
    id       INTEGER PRIMARY KEY,
    call_id  INTEGER NOT NULL REFERENCES calls(id) ON DELETE CASCADE,
    t        REAL NOT NULL,
    title    TEXT NOT NULL
);

CREATE TABLE glossary_client (
    id              INTEGER PRIMARY KEY,
    client_id       INTEGER NOT NULL REFERENCES clients(id) ON DELETE CASCADE,
    kind            TEXT NOT NULL CHECK (kind IN ('term', 'replace')),
    pattern         TEXT NOT NULL,
    replacement     TEXT,
    case_sensitive  INTEGER NOT NULL DEFAULT 0,
    created_at      TEXT NOT NULL,
    source_edit_id  INTEGER
);

-- Toda alteração feita pela UI, pela CLI ou pela importação; base do desfazer.
CREATE TABLE edit_history (
    id         INTEGER PRIMARY KEY,
    call_id    INTEGER NOT NULL REFERENCES calls(id) ON DELETE CASCADE,
    entity     TEXT NOT NULL CHECK (entity IN ('block_text', 'block_speaker', 'call_title', 'speaker_name')),
    entity_id  INTEGER NOT NULL,
    old_value  TEXT,
    new_value  TEXT,
    origin     TEXT NOT NULL CHECK (origin IN ('ui', 'cli', 'import')),
    at         TEXT NOT NULL,
    undone_at  TEXT
);
CREATE INDEX edit_history_call ON edit_history(call_id, id);

-- Busca global. remove_diacritics: "relatorio" encontra "relatório".
CREATE VIRTUAL TABLE blocks_fts USING fts5(
    text, content = 'blocks', content_rowid = 'id', tokenize = 'unicode61 remove_diacritics 2'
);
CREATE TRIGGER blocks_ai AFTER INSERT ON blocks BEGIN
    INSERT INTO blocks_fts(rowid, text) VALUES (new.id, new.text);
END;
CREATE TRIGGER blocks_ad AFTER DELETE ON blocks BEGIN
    INSERT INTO blocks_fts(blocks_fts, rowid, text) VALUES ('delete', old.id, old.text);
END;
CREATE TRIGGER blocks_au AFTER UPDATE OF text ON blocks BEGIN
    INSERT INTO blocks_fts(blocks_fts, rowid, text) VALUES ('delete', old.id, old.text);
    INSERT INTO blocks_fts(rowid, text) VALUES (new.id, new.text);
END;

CREATE VIRTUAL TABLE calls_fts USING fts5(
    title, content = 'calls', content_rowid = 'id', tokenize = 'unicode61 remove_diacritics 2'
);
CREATE TRIGGER calls_ai AFTER INSERT ON calls BEGIN
    INSERT INTO calls_fts(rowid, title) VALUES (new.id, new.title);
END;
CREATE TRIGGER calls_ad AFTER DELETE ON calls BEGIN
    INSERT INTO calls_fts(calls_fts, rowid, title) VALUES ('delete', old.id, old.title);
END;
CREATE TRIGGER calls_au AFTER UPDATE OF title ON calls BEGIN
    INSERT INTO calls_fts(calls_fts, rowid, title) VALUES ('delete', old.id, old.title);
    INSERT INTO calls_fts(rowid, title) VALUES (new.id, new.title);
END;
"#,
// 2 (fase 2): lote de alterações desfeitas juntas (glossário aplicado à chamada).
// `batch_id` é único dentro da biblioteca; `NULL` = edição avulsa.
r#"
ALTER TABLE edit_history ADD COLUMN batch_id INTEGER;
ALTER TABLE edit_history ADD COLUMN batch_kind TEXT;
CREATE INDEX edit_history_batch ON edit_history(batch_id) WHERE batch_id IS NOT NULL;
"#,
// 3 (fase 3): estado da transcrição por chamada. Uma gravação nasce SEM transcrição:
// `transcription_state = 'pending'` (a fase 4 consome a fila: pending → running → done | failed;
// pode acrescentar uma tabela `jobs` sem mexer nesta coluna). Chamadas já existentes (importadas, com
// transcrição) ficam 'done' pelo DEFAULT; 'pending' só vale para chamadas sem transcrição ainda.
r#"
ALTER TABLE calls ADD COLUMN transcription_state TEXT NOT NULL DEFAULT 'done'
    CHECK (transcription_state IN ('pending', 'running', 'done', 'failed'));
ALTER TABLE calls ADD COLUMN transcription_error TEXT;
CREATE INDEX calls_transcription_open ON calls(transcription_state) WHERE transcription_state <> 'done';
"#,
// 4 (fase 4): bruto da transcrição (segmentos por trilha, turnos da diarização, energia) indexado por
// `job_id` (id de `app.db.transcription_jobs`, único no app). Serve de staging durante a tarefa (cada janela
// do Whisper é gravada na hora → retomável) e fica depois da versão criada, para `rediarize`/`resegment`
// sem retranscrever. `transcripts.raw_job_id` liga a versão ao seu bruto e é ÚNICO: impede versão duplicada
// se a tarefa for repetida depois de a versão já ter sido criada.
r#"
ALTER TABLE transcripts ADD COLUMN raw_job_id INTEGER;
CREATE UNIQUE INDEX transcripts_raw_job ON transcripts(raw_job_id) WHERE raw_job_id IS NOT NULL;

-- etapa concluída (o `result` do worker chegou); sem linha = a etapa recomeça (ASR: do último `t_end`)
CREATE TABLE tx_stage (
    job_id     INTEGER NOT NULL,
    stage      TEXT NOT NULL CHECK (stage IN ('asr_sys', 'asr_mic', 'energy', 'diarize')),
    done_at    TEXT NOT NULL,
    info_json  TEXT,
    PRIMARY KEY (job_id, stage)
);
-- tempos SEMPRE do arquivo da trilha (o offset mic×sys é aplicado só na montagem)
CREATE TABLE tx_segments (
    job_id      INTEGER NOT NULL,
    track       TEXT NOT NULL CHECK (track IN ('mic', 'sys')),
    seq         INTEGER NOT NULL,
    t_start     REAL NOT NULL,
    t_end       REAL NOT NULL,
    text        TEXT NOT NULL,
    words_json  TEXT,                         -- [[inicio, fim, "palavra"], ...] ou NULL
    PRIMARY KEY (job_id, track, seq)
);
CREATE TABLE tx_turns (
    job_id   INTEGER NOT NULL,
    seq      INTEGER NOT NULL,
    t_start  REAL NOT NULL,
    t_end    REAL NOT NULL,
    cluster  INTEGER NOT NULL,                -- rótulo bruto do worker (0, 1, ...), antes da fusão
    PRIMARY KEY (job_id, seq)
);
CREATE TABLE tx_energy (
    job_id   INTEGER NOT NULL,
    track    TEXT NOT NULL CHECK (track IN ('mic', 'sys')),
    step_ms  INTEGER NOT NULL,
    db_json  TEXT NOT NULL,                   -- [dBFS por passo], 1 casa decimal
    PRIMARY KEY (job_id, track)
);
-- auditoria do filtro de vazamento (segmentos do mic descartados)
CREATE TABLE bleed_removals (
    id             INTEGER PRIMARY KEY,
    transcript_id  INTEGER NOT NULL REFERENCES transcripts(id) ON DELETE CASCADE,
    t_start        REAL NOT NULL,
    t_end          REAL NOT NULL,
    text           TEXT NOT NULL,
    containment    REAL,
    margin_db      REAL,
    reason         TEXT NOT NULL CHECK (reason IN ('text_and_energy', 'energy_short'))
);
CREATE INDEX bleed_removals_transcript ON bleed_removals(transcript_id);
"#];
