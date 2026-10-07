// Backend falso para abrir a UI no navegador (`npm run dev`) e testar com agent-browser.
// Dados 100% sintéticos. Nunca é carregado dentro do Tauri.
import { mockIPC } from '@tauri-apps/api/mocks'
import { emit } from '@tauri-apps/api/event'
import { TRANSCRIPTION_DEFAULTS } from './api'
import type { ApplyReport, AudioDeletion, DeleteMode, Deletion, AudioEntry, BleedRemoval, BlockedBy, AudioCut, BlockChange, BlockInfo, BlocksChange, CutsChange, BlockSuggestion, CallDetail, ClientInfo, DeviceInfo, GlossaryImportEntry, HistoryEntry, Hit, JobInfo, JobKind, JobOptions, JobProgress, JobStage, LibraryInfo, LevelsEvent, ModelStatus, PauseReason, QueueStatus, Orphan, RecordingInfo, RecordStatus, Rule, SpeakerInfo, StreamChoice, StreamLevel, StreamStatus } from './api'

type Args = Record<string, any>

/** Corte guardado como no banco: removido = `removed` (o histórico aponta para ele por id e o desfazer o traz de volta). */
interface CutRow { id: number; t_start: number; t_end: number; block_id: number | null; created_at: string; removed: boolean }

interface Call extends Omit<CallDetail, 'library_name' | 'words' | 'preview' | 'edited_blocks' | 'transcript_id' | 'deleted_blocks' | 'cuts'> {
  cutRows?: CutRow[]
  /** null = chamada recém-gravada, sem transcrição (RECORDING_CONTRACT §5) */
  transcript_id: number | null
  /** blocos/falantes das versões inativas, por id de transcrição (trocados por set_active_transcript) */
  other?: Record<number, { blocks: BlockInfo[]; speakers: SpeakerInfo[] }>
}

const now = () => new Date().toISOString().slice(0, 19)
let seq = 100
const libs: LibraryInfo[] = [
  { id: 1, name: '', kind: 'inbox', path: '/dados/inbox', available: true, call_count: 0, unassigned_count: 0 },
  { id: 2, name: 'Empresa Exemplo', kind: 'company', path: '/dados/Empresa Exemplo', available: true, call_count: 0, unassigned_count: 0 },
]
const clients: ClientInfo[] = [{ id: 1, library_id: 2, name: 'Cliente Alfa', slug: 'cliente-alfa', call_count: 0 }]
// sobrevive a recarregar a página (como o app.db real); no navegador fica em localStorage
const SETTINGS_KEY = 'mock-settings'
const settings: Record<string, string> = (() => { try { return JSON.parse(localStorage.getItem(SETTINGS_KEY) ?? '{}') } catch { return {} } })()
const history: (HistoryEntry & { library_id: number })[] = []

function mkCall(library_id: number, id: number, key: string, title: string, client_id: number | null, lines: [number, string, string][]): Call {
  const speakers: SpeakerInfo[] = []
  const sid = (label: string) => {
    let s = speakers.find(x => x.label === label)
    if (!s) speakers.push((s = { id: ++seq, track: label === 'Eu' ? 'mic' : 'sys', label, name: null }))
    return s.id
  }
  const blocks: BlockInfo[] = lines.map(([t, spk, text], i) => ({
    id: ++seq, seq: i + 1, t_start: t, t_end: i + 1 < lines.length ? Math.max(t + 1, Math.min(lines[i + 1][0] - 1, t + 30)) : t + 25, speaker_id: sid(spk), text, original_text: text, edited: false, deleted_at: null,
  }))
  const date = key.slice(5, 15), time = key.slice(16).replace(/-/g, ':')
  return {
    library_id, id, key, title, client_id, client_name: null, started_at: `${date}T${time}`,
    duration_s: lines[lines.length - 1][0] + 30, versions: 1, has_audio: true, language: 'pt', expected_speakers: null,
    transcription_state: 'done', transcription_error: null,
    transcript_id: 1, transcripts: [{ id: 1, version: 1, model: 'large-v3-turbo', engine: 'faster-whisper', source_file: key + '.txt', created_at: now(), is_active: true, has_raw: false }],
    speakers, blocks, chapters: [], audio: { mic_path: 'mic.flac', sys_path: 'sys.flac', deleted_at: null },
  }
}

/** Chamada criada pela gravação (§5): sem transcrição, sem blocos/falantes/capítulos, `versions: 0`, estado 'pending'. */
function mkPending(library_id: number, id: number, key: string, title: string, client_id: number | null, duration_s: number, opts: { expected_speakers?: number | null; language?: string | null } = {}): Call {
  const date = key.slice(5, 15), time = key.slice(16).replace(/-/g, ':')
  return {
    library_id, id, key, title, client_id, client_name: null, started_at: `${date}T${time}`,
    duration_s, versions: 0, has_audio: true, language: opts.language ?? null, expected_speakers: opts.expected_speakers ?? null,
    transcription_state: 'pending', transcription_error: null,
    transcript_id: null, transcripts: [], speakers: [], blocks: [], chapters: [],
    audio: { mic_path: 'mic.flac', sys_path: 'sys.flac', deleted_at: null },
  }
}

// 2ª versão da transcrição (modelo diferente) para testar a troca de versão
function addVersion(c: Call, model: string, lines: [number, string, string][]) {
  const v2 = mkCall(c.library_id, c.id, c.key, c.title, c.client_id, lines)
  const id = 2
  c.transcripts.push({ id, version: 2, model, engine: 'faster-whisper', source_file: c.key + '_v2.txt', created_at: now(), is_active: false, has_raw: false })
  c.other = { ...(c.other ?? {}), [id]: { blocks: v2.blocks, speakers: v2.speakers } }
}

const filler = (n: number) =>
  Array.from({ length: n }, (_, i): [number, string, string] => [
    40 + i * 37,
    i % 3 === 0 ? 'Eu' : 'Outros',
    `Trecho sintético número ${i + 1} sobre o relatório de testes, com o serviço de notificações e a fila de pagamentos.`,
  ])

const calls: Call[] = [
  mkCall(1, 1, 'call_2026-03-02_10-00-00', 'Planejamento da sprint', null, [
    [2, 'Outros', 'Bom dia, vamos começar pelo Gate Wei Service que caiu ontem.'],
    [15, 'Eu', 'Eu olhei os logs, foi timeout no gateway.'],
    ...filler(30),
  ]),
  mkCall(1, 2, 'call_2026-03-01_15-30-00', '', null, [[0, 'Outros', 'Chamada curta sem título.'], [10, 'Eu', 'Ok, obrigado.']]),
  mkCall(2, 3, 'call_2026-02-20_09-15-00', 'Revisão do relatório mensal', 1, [
    [5, 'Speaker 1', 'O relatório de fevereiro ficou pronto.'],
    [20, 'Speaker 2', 'Ótimo, vamos revisar os números de vendas.'],
    ...filler(8),
  ]),
]

// uma chamada gravada e ainda sem transcrição, para ver o selo/estado vazio em qualquer idioma sem gravar
calls.push(mkPending(1, 4, 'call_2026-03-03_09-00-00', '', null, 754, { expected_speakers: 3, language: 'pt' }))

// ?backlog=N: mais N chamadas gravadas e pendentes, para ver "N na frente" na fila
for (let i = 1; i <= Math.min(5, Number(new URLSearchParams(location.search).get('backlog')) || 0); i++) {
  calls.push(mkPending(1, 40 + i, `call_2026-03-02_1${i}-00-00`, `Reunião de alinhamento ${i}`, null, 600 + i * 120, { language: 'pt' }))
}

// sem áudio: importada só do texto (nenhuma trilha) e com o áudio apagado (`audio_deleted_at`); a tela não mostra o player
{
  const noAudio = mkCall(1, 6, 'call_2026-02-27_16-00-00', 'Importada sem áudio', null, [[4, 'Outros', 'Esta chamada veio só do texto.'], [20, 'Eu', 'Sem trilha de áudio para tocar.']])
  Object.assign(noAudio, { has_audio: false, audio: { mic_path: null, sys_path: null, deleted_at: null } })
  const gone = mkCall(1, 7, 'call_2026-02-26_11-00-00', 'Áudio apagado', null, [[4, 'Outros', 'O áudio desta chamada foi apagado.'], [20, 'Eu', 'O texto continua aqui.']])
  Object.assign(gone, { has_audio: false, audio: { mic_path: null, sys_path: null, deleted_at: '2026-03-01T10:00:00' } })
  calls.push(noAudio, gone)
}

addVersion(calls[0], 'large-v3', [
  [2, 'Outros', 'Bom dia, vamos começar pelo serviço que caiu ontem (segunda versão).'],
  [15, 'Eu', 'Eu olhei os logs, foi timeout no gateway de pagamento.'],
  [60, 'Outros', 'Versão dois da transcrição, com menos trechos.'],
])

function lib(id: number) {
  const l = libs.find(l => l.id === id)
  if (!l) throw { code: 'not_found', detail: `library ${id}` }
  return l
}
function findCall(library_id: number, id: number) {
  const c = calls.find(c => c.library_id === library_id && c.id === id)
  if (!c) throw { code: 'not_found', detail: `call ${library_id}:${id}` }
  return c
}
function findBlock(library_id: number, block_id: number) {
  for (const c of calls) {
    if (c.library_id !== library_id) continue
    const b = c.blocks.find(b => b.id === block_id)
    if (b) return { c, b }
  }
  throw { code: 'not_found', detail: `block ${block_id}` }
}
/** blocos vivos (os excluídos ficam em `c.blocks` com `deleted_at`, como no banco) */
const live = (c: Call) => c.blocks.filter(b => !b.deleted_at)
function summary(c: Call) {
  const words = live(c).reduce((n, b) => n + b.text.split(/\s+/).filter(Boolean).length, 0)
  const preview = live(c).slice(0, 3).map(b => b.text).join(' ').slice(0, 200)
  const client = clients.find(x => x.id === c.client_id)
  const { speakers: _s, blocks: _b, chapters: _c, transcripts: _t, audio: _a, other: _o, cutRows: _cr, ...rest } = c
  return { ...rest, client_name: client?.name ?? null, words, preview, edited_blocks: live(c).filter(b => b.edited).length }
}
function record(library_id: number, call_id: number, entity: HistoryEntry['entity'], entity_id: number, old_value: string | null, new_value: string | null, batch?: { id: number; size: number; kind?: 'glossary' | 'delete' | 'restore' | 'cut_add' | 'cut_remove' }) {
  const id = ++seq
  history.push({
    library_id, id, call_id, entity, entity_id, old_value, new_value, origin: 'ui', at: now(), undone_at: null,
    batch_id: batch?.id ?? null, batch_kind: batch ? batch.kind ?? 'glossary' : null, batch_size: batch?.size ?? null,
  })
  return id
}
// ---- cortes de áudio (#23): as mesmas regras do núcleo (cuts.rs)
type Span = [number, number]
const MIN_COVERAGE = 0.5
const cutRows = (c: Call) => (c.cutRows ??= [])
const liveRows = (c: Call) => cutRows(c).filter(r => !r.removed)
const mergeSpans = (v: Span[]): Span[] => {
  const out: Span[] = []
  for (const [s, e] of [...v].sort((a, b) => a[0] - b[0] || a[1] - b[1])) {
    const last = out[out.length - 1]
    if (last && s <= last[1]) last[1] = Math.max(last[1], e)
    else out.push([s, e])
  }
  return out
}
const covered = (m: Span[], a: number, b: number) => m.reduce((n, [s, e]) => n + Math.max(0, Math.min(b, e) - Math.max(a, s)), 0)
/** Metade ou mais da duração dentro dos cortes (bloco sem duração: vale o instante). */
const isCovered = (m: Span[], a: number, b: number) => (b <= a ? m.some(([s, e]) => a >= s && a < e) : covered(m, a, b) >= (b - a) * MIN_COVERAGE - 1e-9)
const subtractSpans = (m: Span[], [s, e]: Span): Span[] => {
  const out: Span[] = []
  let at = s
  for (const [ms, me] of m) {
    if (me <= at || ms >= e) continue
    if (ms > at) out.push([at, ms])
    at = Math.max(at, me)
  }
  if (at < e) out.push([at, e])
  return out
}
const round3 = (v: number) => Math.round(v * 1000) / 1000
const toCut = (c: Call, r: CutRow): AudioCut => ({
  id: r.id, t_start: r.t_start, t_end: r.t_end, block_id: r.block_id,
  block_seq: r.block_id == null ? null : c.blocks.find(b => b.id === r.block_id)?.seq ?? null, created_at: r.created_at,
})
const cutList = (c: Call) => liveRows(c).sort((a, b) => a.t_start - b.t_start || a.id - b.id).map(r => toCut(c, r))
const unionOf = (rows: CutRow[]) => mergeSpans(rows.map(r => [r.t_start, r.t_end] as Span))
function audioProblem(c: Call) {
  if (c.audio.deleted_at) throw bad('audio_deleted', c.key)
  if (!c.audio.mic_path && !c.audio.sys_path) throw bad('no_audio', c.key)
}
/** A exclusão veio de um salvamento de cortes? (o último `block_deleted` válido do bloco está num lote `cut_add`) */
function deletedByCut(library_id: number, c: Call, block_id: number) {
  const h = history.filter(x => x.library_id === library_id && x.call_id === c.id && x.entity === 'block_deleted' && x.entity_id === block_id && !x.undone_at).pop()
  return !!h && h.new_value != null && h.batch_kind === 'cut_add'
}
const asBatch = (n: number, kind: 'delete' | 'restore' | 'cut_add' | 'cut_remove') => (n ? { id: ++seq, size: n, kind } : undefined)

/** Calcula e (se `write`) grava `add_cuts`: pedidos fundidos e ajustados à chamada, cortados pelos cortes manuais que já existem,
 * e exclusão dos trechos com metade ou mais dentro da união de todos os cortes, num lote só. */
function addCuts(a: Args, write: boolean): CutsChange {
  const c = findCall(a.libraryId, a.callId)
  audioProblem(c)
  const limit = Math.max(c.duration_s, ...c.blocks.map(b => b.t_end))
  const req = (a.spans as Span[])
  if (!req.length) throw bad('invalid', 'no cuts given')
  const spans = mergeSpans(req.map(([s, e]) => {
    if (!Number.isFinite(s) || !Number.isFinite(e) || e <= s) throw bad('invalid', `cut ${s}..${e}`)
    if (s >= limit) throw bad('invalid', `cut ${s}..${e} is past the end of the call`)
    return [round3(Math.max(0, s)), round3(Math.min(limit, e))] as Span
  }))
  const manual = mergeSpans(liveRows(c).filter(r => r.block_id == null).map(r => [r.t_start, r.t_end] as Span))
  const fresh: Span[] = [], skipped: Span[] = []
  for (const sp of spans) { const rest = subtractSpans(manual, sp); if (rest.length) fresh.push(...rest); else skipped.push(sp) }
  const union = mergeSpans([...unionOf(liveRows(c)), ...fresh])
  const gone = live(c).filter(b => isCovered(union, b.t_start, b.t_end))
  const batch = asBatch(fresh.length + gone.length, 'cut_add')
  const added: AudioCut[] = []
  if (write && batch) {
    for (const [s, e] of fresh) {
      const r: CutRow = { id: ++seq, t_start: s, t_end: e, block_id: null, created_at: now(), removed: false }
      cutRows(c).push(r)
      record(a.libraryId, c.id, 'audio_cut', r.id, r.created_at, null, batch)
      added.push(toCut(c, r))
    }
    for (const b of gone) { b.deleted_at = now(); record(a.libraryId, c.id, 'block_deleted', b.id, null, b.deleted_at, batch) }
  } else added.push(...fresh.map(([s, e], i) => ({ id: -(i + 1), t_start: s, t_end: e, block_id: null, block_seq: null, created_at: now() })))
  const after: AudioCut[] = write ? cutList(c) : [...cutList(c), ...added].sort((x, y) => x.t_start - y.t_start)
  return { call_id: c.id, added, removed: [], skipped, deleted_blocks: gone.map(b => ({ ...b })), restored_blocks: [], cuts: after }
}

function removeCut(a: Args): CutsChange {
  const c = findCall(a.libraryId, a.callId)
  const row = liveRows(c).find(r => r.id === a.cutId)
  if (!row) throw bad('not_found', `cut ${a.cutId}`)
  if (row.block_id != null && c.blocks.some(b => b.id === row.block_id)) throw bad('conflict', `cut ${a.cutId} belongs to a deleted passage: restore the passage instead`)
  const rest = unionOf(liveRows(c).filter(r => r !== row))
  const back = c.blocks.filter(b => b.deleted_at && deletedByCut(a.libraryId, c, b.id) && !isCovered(rest, b.t_start, b.t_end))
  const batch = asBatch(1 + back.length, 'cut_remove')!
  row.removed = true
  record(a.libraryId, c.id, 'audio_cut', row.id, null, now(), batch)
  for (const b of back) { const old = b.deleted_at; b.deleted_at = null; record(a.libraryId, c.id, 'block_deleted', b.id, old, null, batch) }
  return { call_id: c.id, added: [], removed: [toCut(c, row)], skipped: [], deleted_blocks: [], restored_blocks: back.map(b => ({ ...b })), cuts: cutList(c) }
}

/** delete_blocks/restore_blocks: lote único no histórico; idempotente (já no estado pedido = `unchanged`).
 * Excluir cria um corte ligado a cada trecho (se a chamada tem áudio); restaurar tira os cortes ligados. */
function setDeleted(a: Args, del: boolean): BlocksChange {
  const ids = [...new Set<number>(a.blockIds)]
  if (!ids.length) throw { code: 'invalid', detail: 'no blocks given' }
  const found = ids.map(id => findBlock(a.libraryId, id)) // um id ruim desfaz tudo (nada foi gravado ainda)
  const todo = found.filter(({ b }) => !!b.deleted_at !== del)
  const hasAudio = (c: Call) => !c.audio.deleted_at && !!(c.audio.mic_path || c.audio.sys_path)
  const linked = (c: Call, b: BlockInfo) => liveRows(c).filter(r => r.block_id === b.id)
  const nCuts = todo.reduce((n, { c, b }) => n + (del ? (hasAudio(c) && b.t_end > b.t_start ? 1 : 0) : linked(c, b).length), 0)
  const batch = asBatch(todo.length + nCuts, del ? 'delete' : 'restore')
  const out: BlocksChange = { changed: [], unchanged: [], cuts_added: [], cuts_removed: [] }
  for (const { c, b } of found) {
    if (!!b.deleted_at === del) { out.unchanged.push({ ...b }); continue }
    const old = b.deleted_at
    b.deleted_at = del ? now() : null
    record(a.libraryId, c.id, 'block_deleted', b.id, old, b.deleted_at, batch)
    out.changed.push({ ...b })
    if (del && hasAudio(c) && b.t_end > b.t_start) {
      const r: CutRow = { id: ++seq, t_start: b.t_start, t_end: b.t_end, block_id: b.id, created_at: now(), removed: false }
      cutRows(c).push(r)
      record(a.libraryId, c.id, 'audio_cut', r.id, r.created_at, null, batch)
      out.cuts_added.push(toCut(c, r))
    } else if (!del) {
      for (const r of linked(c, b)) { r.removed = true; record(a.libraryId, c.id, 'audio_cut', r.id, null, now(), batch); out.cuts_removed.push(toCut(c, r)) }
    }
  }
  return out
}
const norm = (s: string) => s.replace(/\s+/g, ' ').trim()
const fold = (s: string) => s.normalize('NFD').replace(/\p{M}/gu, '').toLowerCase()

const handlers: Record<string, (a: Args) => unknown> = {
  bootstrap: () => ({ data_dir: '/dados', system_language: 'pt-BR', inbox_id: 1, settings: effSettings(), libraries: handlers.libraries({}) }),
  libraries: () => libs.map(l => ({
    ...l,
    call_count: calls.filter(c => c.library_id === l.id).length,
    unassigned_count: calls.filter(c => c.library_id === l.id && c.client_id === null).length,
  })),
  add_library: a => { const id = ++seq; libs.push({ id, name: a.name, kind: 'company', path: a.path, available: true, call_count: 0, unassigned_count: 0 }); return id },
  rename_library: a => { lib(a.libraryId).name = a.name },
  remove_library: a => { libs.splice(libs.indexOf(lib(a.libraryId)), 1) },
  clients: a => clients.filter(c => c.library_id === a.libraryId).map(c => ({ ...c, call_count: calls.filter(x => x.library_id === c.library_id && x.client_id === c.id).length })),
  add_client: a => { const c = { id: ++seq, library_id: a.libraryId, name: norm(a.name), slug: fold(a.name).replace(/\W+/g, '-'), call_count: 0 }; clients.push(c); return c },
  rename_client: a => { const c = clients.find(c => c.id === a.clientId); if (c) c.name = a.name },
  calls: a => calls
    .filter(c => a.libraryId == null || c.library_id === a.libraryId)
    .filter(c => (a.clientId != null ? c.client_id === a.clientId : !a.unassigned || c.client_id === null))
    .sort((x, y) => y.started_at.localeCompare(x.started_at))
    .map(summary),
  call_detail: a => {
    const c = findCall(a.libraryId, a.callId)
    const { other: _o, cutRows: _cr, ...full } = c
    if (a.transcriptId != null && c.transcript_id == null) throw { code: 'not_found', detail: `transcript ${a.transcriptId}` }
    return { ...full, ...summary(c), library_name: lib(c.library_id).name, blocks: live(c).map(b => ({ ...b })), deleted_blocks: c.blocks.filter(b => b.deleted_at).map(b => ({ ...b })), speakers: c.speakers.map(s => ({ ...s })), cuts: cutList(c) }
  },
  set_block_text: a => {
    const { c, b } = findBlock(a.libraryId, a.blockId)
    const t = norm(a.text)
    if (!t) throw { code: 'invalid', detail: 'text is empty' }
    let edit_id: number | null = null
    const old = b.text
    if (t !== b.text) { edit_id = record(a.libraryId, c.id, 'block_text', b.id, b.text, t); b.text = t; b.edited = t !== b.original_text }
    return { ...b, edit_id, suggestions: edit_id ? suggest(a.libraryId, c, b.id, old, t) : [] }
  },
  preview_cuts: a => addCuts(a, false),
  add_cuts: a => addCuts(a, true),
  remove_cut: a => removeCut(a),
  delete_blocks: a => setDeleted(a, true),
  restore_blocks: a => setDeleted(a, false),
  revert_block: a => { const { b } = findBlock(a.libraryId, a.blockId); return handlers.set_block_text({ ...a, text: b.original_text }) },
  set_title: a => {
    const c = findCall(a.libraryId, a.callId)
    const t = norm(a.title)
    if (t !== c.title) { record(a.libraryId, c.id, 'call_title', c.id, c.title, t); c.title = t }
    return summary(c)
  },
  rename_speaker: a => {
    for (const c of calls) {
      const s = c.speakers.find(s => s.id === a.speakerId)
      if (!s) continue
      const n = a.name ? norm(a.name) : null
      if (n !== s.name) { record(a.libraryId, c.id, 'speaker_name', s.id, s.name, n); s.name = n }
      return { ...s }
    }
    throw { code: 'not_found', detail: 'speaker' }
  },
  set_block_speaker: a => {
    const { c, b } = findBlock(a.libraryId, a.blockId)
    if (b.speaker_id !== a.speakerId) { record(a.libraryId, c.id, 'block_speaker', b.id, String(b.speaker_id), String(a.speakerId)); b.speaker_id = a.speakerId }
    return { ...b }
  },
  set_active_transcript: a => {
    const c = findCall(a.libraryId, a.callId)
    if (c.transcript_id == null) throw { code: 'not_found', detail: `transcript ${a.transcriptId}` }
    const alt = c.other?.[a.transcriptId]
    if (!alt) { if (a.transcriptId === c.transcript_id) return null; throw { code: 'not_found', detail: `transcript ${a.transcriptId}` } }
    c.other = { ...c.other, [c.transcript_id]: { blocks: c.blocks, speakers: c.speakers } }
    delete c.other[a.transcriptId]
    c.blocks = alt.blocks; c.speakers = alt.speakers
    c.transcript_id = a.transcriptId
    c.transcripts.forEach(v => (v.is_active = v.id === a.transcriptId))
    return null
  },
  history: a => history.filter(h => h.library_id === a.libraryId && h.call_id === a.callId).slice().reverse(),
  undo: a => {
    const h = history.filter(h => h.library_id === a.libraryId && h.call_id === a.callId && !h.undone_at && h.origin !== 'import').pop()
    if (!h) return null
    const c = findCall(a.libraryId, a.callId)
    // lote inteiro de uma vez (a ordem inversa garante o texto de antes do lote)
    const group = h.batch_id ? history.filter(x => x.batch_id === h.batch_id && !x.undone_at).reverse() : [h]
    for (const e of group) {
      if (e.entity === 'block_text') { const b = c.blocks.find(b => b.id === e.entity_id)!; b.text = e.old_value!; b.edited = b.text !== b.original_text }
      if (e.entity === 'call_title') c.title = e.old_value ?? ''
      if (e.entity === 'speaker_name') c.speakers.find(s => s.id === e.entity_id)!.name = e.old_value
      if (e.entity === 'block_speaker') c.blocks.find(b => b.id === e.entity_id)!.speaker_id = Number(e.old_value)
      if (e.entity === 'block_deleted') c.blocks.find(b => b.id === e.entity_id)!.deleted_at = e.old_value
      if (e.entity === 'audio_cut') cutRows(c).find(r => r.id === e.entity_id)!.removed = e.old_value != null
      e.undone_at = now()
    }
    return h
  },
  search_all: a => {
    const terms = fold(a.query).split(/[^\p{L}\p{N}]+/u).filter(Boolean)
    if (!terms.length) return []
    const hit = (s: string) => terms.every(t => fold(s).includes(t))
    const mark = (s: string) => s.replace(new RegExp(`(${terms.join('|')})`, 'gi'), '\u0002$1\u0003')
    const out = []
    for (const c of calls) {
      if (hit(c.title)) out.push({ library_id: c.library_id, call_id: c.id, call_key: c.key, call_title: c.title, started_at: c.started_at, block_id: null, t_start: null, snippet: mark(c.title), rank: -2 })
      for (const b of live(c)) if (hit(b.text)) out.push({ library_id: c.library_id, call_id: c.id, call_key: c.key, call_title: c.title, started_at: c.started_at, block_id: b.id, t_start: b.t_start, snippet: mark(b.text), rank: -1 })
    }
    return out.slice(0, a.limit)
  },
  assign: a => {
    const c = findCall(a.libraryId, a.callId)
    c.library_id = a.toLibraryId
    c.client_id = a.clientId
    return { library_id: c.library_id, call_id: c.id }
  },
  set_setting: a => {
    // mesma lista de chaves de gui.rs::set_setting
    // gui.rs::set_setting; `record_shortcut` NÃO está na lista (só `record_set_shortcut` escreve)
    if (!['language', 'me_name', 'transcription_language', ...Object.keys(TRANSCRIPTION_DEFAULTS), 'last_library_id', 'last_client_id', 'record_library_id', 'record_client_id', 'record_mic', 'record_sys', 'record_bar_on_start'].includes(a.key)) throw { code: 'invalid', detail: `unknown setting ${a.key}` }
    if (a.value == null || !String(a.value).trim()) delete settings[a.key]; else settings[a.key] = a.value
    try { localStorage.setItem(SETTINGS_KEY, JSON.stringify(settings)) } catch {}
  },
  reclaimable: () => ({ total_bytes: 1_234_567_890, files: [{ library_id: 1, call_key: calls[0].key, kind: 'sys', path: '/origem/x.sys.wav', size: 1_234_567_890 }] }),
  import_preview: () => ({ dry_run: true, items: [
    { key: 'call_2026-03-05_11-00-00', status: 'new', reason: null, library_id: 1, call_id: null, versions_added: [1, 2], edits_applied: 0, glossary_replacements: 0, audio_pending: ['mic', 'sys'], audio_converted: [], audio_errors: [] },
    { key: 'call_2026-03-04_11-00-00', status: 'skipped', reason: 'empty: call_2026-03-04_11-00-00.txt', library_id: null, call_id: null, versions_added: [], edits_applied: 0, glossary_replacements: 0, audio_pending: [], audio_converted: [], audio_errors: [] },
  ] }),
  import_start: async (a: Args) => {
    const { emit } = await import('@tauri-apps/api/event')
    for (let i = 0; i <= 10; i++) {
      await new Promise(r => setTimeout(r, 80))
      await emit('import-progress', { stage: 'audio', index: 0, total: 2, key: 'call_2026-03-05_11-00-00', track: 'mic', done: i * 10, of: 100 })
    }
    // a chamada "nova" passa a existir (na caixa de entrada, ou onde o usuário escolheu)
    const key = 'call_2026-03-05_11-00-00'
    if (!calls.some(c => c.key === key)) {
      const c = mkCall(a.libraryId ?? 1, ++seq, key, '', a.clientId ?? null, filler(6))
      calls.push(c)
    }
    const items = (handlers.import_preview({}) as any).items.map((i: any) =>
      i.status === 'new' ? { ...i, status: 'new', glossary_replacements: 3, audio_pending: [], audio_converted: ['mic', 'sys'] } : i)
    await emit('import-done', { ok: true, report: { dry_run: false, items } })
    return null
  },
}

// ---------------------------------------------------------------- glossário
// Duas camadas (global / cliente); ids em espaços separados. Motor simplificado: palavra inteira,
// sem caixa, uma passada, o padrão mais longo vence.
const rules: Rule[] = []
let gSeq = 0
const cSeq = new Map<number, number>() // por biblioteca
const rkey = (r: Pick<Rule, 'kind' | 'pattern'>) => r.kind + '|' + fold(norm(r.pattern))
const layerOf = (scope: string, libraryId: number | null) => rules.filter(r => r.scope === scope && (scope === 'global' || r.library_id === libraryId))

function addRule(a: Args): Rule {
  const scope = a.scope as Rule['scope']
  if (scope !== 'global' && scope !== 'client') throw { code: 'invalid', detail: 'unknown scope' }
  if (a.kind !== 'term' && a.kind !== 'replace') throw { code: 'invalid', detail: 'unknown kind' }
  const pattern = norm(a.pattern ?? '')
  const replacement = a.kind === 'replace' ? norm(a.replacement ?? '') : null
  if (!pattern || pattern.length > 200) throw { code: 'invalid', detail: 'pattern is empty or too long' }
  if (a.kind === 'replace' && (!replacement || replacement === pattern || replacement.length > 500)) throw { code: 'invalid', detail: 'bad replacement' }
  if (scope === 'client') {
    if (a.libraryId == null) throw { code: 'invalid', detail: 'libraryId is required' }
    if (a.clientId == null || lib(a.libraryId).kind === 'inbox' || !clients.some(c => c.id === a.clientId)) throw { code: 'not_found', detail: 'client' }
  }
  const libraryId = scope === 'client' ? a.libraryId : null
  if (layerOf(scope, libraryId).some(r => r.id !== a.id && rkey(r) === rkey({ kind: a.kind, pattern }))) throw { code: 'conflict', detail: 'rule already exists' }
  let id = a.id
  if (id == null) id = scope === 'global' ? ++gSeq : (cSeq.set(libraryId, (cSeq.get(libraryId) ?? 0) + 1), cSeq.get(libraryId)!)
  return {
    id, scope, library_id: libraryId, client_id: scope === 'client' ? a.clientId : null, kind: a.kind, pattern, replacement,
    case_sensitive: !!a.caseSensitive, created_at: now(), source_edit_id: a.sourceEditId ?? null,
    source_library_id: scope === 'global' && a.sourceEditId != null ? a.libraryId ?? null : null, overridden: false,
  }
}
function findRule(scope: string, libraryId: number | null, id: number) {
  const r = layerOf(scope, libraryId).find(r => r.id === id)
  if (!r) throw { code: 'not_found', detail: 'rule' }
  return r
}
/** Regras em vigor: do cliente primeiro; as globais cobertas por uma do cliente saem com `overridden`. */
function merged(libraryId: number | null, clientId: number | null): Rule[] {
  const globals = layerOf('global', null)
  if (libraryId == null || clientId == null) return globals.map(r => ({ ...r }))
  const own = layerOf('client', libraryId).filter(r => r.client_id === clientId)
  const keys = new Set(own.map(rkey))
  return [...own.map(r => ({ ...r })), ...globals.map(r => ({ ...r, overridden: keys.has(rkey(r)) }))]
}

// regras sintéticas de partida: o Cliente Alfa tem as suas (a chamada 3 dá prévia ao aplicar) e há duas globais
for (const a of [
  { scope: 'client', libraryId: 2, clientId: 1, kind: 'replace', pattern: 'fila de pagamentos', replacement: 'fila de cobrança' },
  { scope: 'client', libraryId: 2, clientId: 1, kind: 'replace', pattern: 'serviço de notificações', replacement: 'Serviço de Alertas', caseSensitive: true },
  { scope: 'client', libraryId: 2, clientId: 1, kind: 'term', pattern: 'Alfa Cobrança' },
  { scope: 'global', kind: 'replace', pattern: 'Gate Wei Service', replacement: 'Gateway Service' },
  { scope: 'global', kind: 'term', pattern: 'Gateway Service' },
]) rules.push(addRule(a))

const edge = (c: string) => /[\p{L}\p{N}]/u.test(c)
function patternRx(pattern: string, flags: string) {
  const body = pattern.split(/\s+/).map(rx).join('\\s+')
  const pre = edge(pattern[0]) ? '(?<![\\p{L}\\p{N}])' : '', post = edge(pattern[pattern.length - 1]) ? '(?![\\p{L}\\p{N}])' : ''
  return new RegExp(pre + body + post, flags + 'u')
}
const rx = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')

function recase(found: string, rep: string) {
  if (found.length > 1 && found === found.toUpperCase() && found !== found.toLowerCase()) return rep.toUpperCase()
  if (/^\p{Lu}/u.test(found) && /^\p{Ll}/u.test(rep)) return rep[0].toUpperCase() + rep.slice(1)
  return rep
}

function applyRules(text: string, eff: Rule[]): { text: string; hits: Hit[] } {
  const reps = eff.filter(r => r.kind === 'replace' && !r.overridden).sort((a, b) => b.pattern.length - a.pattern.length)
  const hits = new Map<Rule, number>()
  let out = '', i = 0
  const all = reps.map(r => ({ r, re: patternRx(r.pattern, r.case_sensitive ? 'g' : 'gi') }))
  while (i < text.length) {
    // o padrão mais longo que casa exatamente nesta posição vence
    let best: { r: Rule; len: number; txt: string } | null = null
    for (const { r, re } of all) {
      re.lastIndex = i
      const m = re.exec(text)
      if (m && m.index === i && (!best || m[0].length > best.len)) best = { r, len: m[0].length, txt: m[0] }
    }
    if (best) { out += recase(best.txt, best.r.replacement!); hits.set(best.r, (hits.get(best.r) ?? 0) + 1); i += best.len }
    else out += text[i++]
  }
  return { text: out, hits: [...hits].map(([r, count]) => ({ scope: r.scope, rule_id: r.id, pattern: r.pattern, replacement: r.replacement!, count })) }
}

/** Menor trecho (em palavras) que mudou entre o texto antigo e o novo. */
function suggest(libraryId: number, c: Call, blockId: number, oldText: string, newText: string): BlockSuggestion[] {
  const x = norm(oldText).split(' '), y = norm(newText).split(' ')
  let p = 0
  while (p < x.length && p < y.length && x[p] === y[p]) p++
  let e = 0
  while (e < x.length - p && e < y.length - p && x[x.length - 1 - e] === y[y.length - 1 - e]) e++
  const trim = (v: string) => v.replace(/^[^\p{L}\p{N}]+|[^\p{L}\p{N}]+$/gu, '')
  const pattern = trim(x.slice(p, x.length - e).join(' ')), replacement = trim(y.slice(p, y.length - e).join(' '))
  if (!pattern || !replacement) return []
  const eff = merged(libraryId, c.client_id)
  if (eff.some(r => r.kind === 'replace' && fold(r.pattern) === fold(pattern))) return []
  const re = patternRx(pattern, 'i')
  const client = clients.find(k => k.id === c.client_id)
  return [{
    pattern, replacement,
    occurrences_in_call: live(c).filter(b => b.id !== blockId && re.test(b.text)).length,
    client: client ? { id: client.id, name: client.name } : null,
  }]
}

const tokens = (t: string) => Math.ceil(t.length / 3) + 1
const SAMPLE_LIST = ['# lista de exemplo (o mock ignora o caminho)', 'Zenit -> Zenith', 'gate way → Gateway', 'Zenith Service', 'Cliente Alfa', 'só o lado esquerdo ->', 'Zenit => Zenith']

Object.assign(handlers, {
  glossary_list: (a: Args) => merged(a.libraryId ?? null, a.clientId ?? null).filter(r => !a.kind || r.kind === a.kind),
  glossary_add: (a: Args) => { const r = addRule(a); rules.push(r); return r },
  glossary_update: (a: Args) => {
    const old = findRule(a.scope, a.libraryId ?? null, a.id)
    const next = addRule({ ...a, clientId: old.client_id, id: old.id, sourceEditId: old.source_edit_id })
    Object.assign(old, next, { created_at: old.created_at })
    return { ...old }
  },
  glossary_remove: (a: Args) => { const r = findRule(a.scope, a.libraryId ?? null, a.id); rules.splice(rules.indexOf(r), 1); return r },
  glossary_promote: (a: Args) => {
    const r = findRule('client', a.libraryId, a.id)
    const twin = layerOf('global', null).find(g => rkey(g) === rkey(r))
    if (twin && (twin.replacement !== r.replacement)) throw { code: 'conflict', detail: 'global rule differs' }
    rules.splice(rules.indexOf(r), 1)
    if (twin) return { ...twin }
    const g = { ...r, id: ++gSeq, scope: 'global' as const, library_id: null, client_id: null, source_library_id: null }
    rules.push(g)
    return g
  },
  glossary_apply: (a: Args): ApplyReport => {
    const c = findCall(a.libraryId, a.callId)
    if (c.transcript_id == null) throw { code: 'not_found', detail: 'transcript' }
    const eff = merged(a.libraryId, c.client_id)
    const changes: BlockChange[] = []
    for (const b of live(c)) {
      const r = applyRules(b.text, eff)
      if (r.text !== b.text) changes.push({ block_id: b.id, seq: b.seq, before: b.text, after: r.text, rules: r.hits })
    }
    const batch = !a.dryRun && changes.length ? ++seq : null
    if (batch) for (const ch of changes) {
      const b = c.blocks.find(b => b.id === ch.block_id)!
      record(a.libraryId, c.id, 'block_text', b.id, b.text, ch.after, { id: batch, size: changes.length })
      b.text = ch.after; b.edited = b.text !== b.original_text
    }
    return {
      call_id: c.id, transcript_id: c.transcript_id!, dry_run: !!a.dryRun, blocks_changed: changes.length,
      replacements: changes.reduce((n, ch) => n + ch.rules.reduce((m, h) => m + h.count, 0), 0), batch_id: batch, changes,
    }
  },
  glossary_import_file: (a: Args) => {
    if (!a.path) throw { code: 'not_found', detail: 'file' }
    if (a.scope === 'client' && (a.libraryId == null || a.clientId == null)) throw { code: 'invalid', detail: 'libraryId is required' }
    const libraryId = a.scope === 'client' ? a.libraryId : null
    const seen = new Set<string>()
    const entries: GlossaryImportEntry[] = []
    SAMPLE_LIST.forEach((raw, i) => {
      const line = raw.trim()
      if (!line || line.startsWith('#')) return
      const m = line.split(/\s*(?:->|→|=>)\s*/)
      const kind = m.length > 1 ? 'replace' : 'term'
      if (a.kind && a.kind !== kind) return
      const pattern = norm(m[0]), replacement = kind === 'replace' ? norm(m[1] ?? '') : null
      const e: GlossaryImportEntry = { line: i + 1, kind, pattern, replacement, status: 'added', reason: null }
      if (kind === 'replace' && (!pattern || !replacement)) Object.assign(e, { status: 'invalid', reason: 'empty_side' })
      else if (seen.has(rkey({ kind, pattern })) || layerOf(a.scope, libraryId).some(r => rkey(r) === rkey({ kind, pattern }))) e.status = 'duplicate'
      else {
        seen.add(rkey({ kind, pattern }))
        if (!a.dryRun) rules.push(addRule({ scope: a.scope, libraryId, clientId: a.clientId, kind, pattern, replacement, caseSensitive: false }))
      }
      entries.push(e)
    })
    const n = (s: string) => entries.filter(e => e.status === s).length
    return { dry_run: !!a.dryRun, added: n('added'), skipped: n('duplicate'), invalid: n('invalid'), entries }
  },
  glossary_suggestions: (a: Args) => { const { c } = findBlock(a.libraryId, a.blockId); return suggest(a.libraryId, c, a.blockId, a.oldText, a.newText) },
  glossary_prompt_terms: (a: Args) => {
    const all = [...new Map(merged(a.libraryId, a.clientId ?? null).filter(r => r.kind === 'term' && !r.overridden).map(r => [fold(r.pattern), r.pattern])).values()]
    const terms: string[] = []
    let used = 0
    for (const t of all) { if (used + tokens(t) > 224) break; terms.push(t); used += tokens(t) }
    return { terms, estimated_tokens: used, budget_tokens: 224 }
  },
})

// ---------------------------------------------------------------- gravação (fase 3)
// Backend falso fiel ao RECORDING_CONTRACT: estado em memória, níveis ~10 Hz como evento `record-levels`,
// finalização com progresso, órfãs, atalho. Opções pela URL (`/?mock=a,b#/...`), úteis porque trocar o idioma recarrega:
//   wayland (Hyprland, sem atalho global) · gnome (Wayland sem Hyprland) · orphans (duas gravações órfãs no boot)
//   nobackend (servidor de áudio ausente) · noinfo (record_info falha)
const flags = new Set((new URLSearchParams(location.search).get('mock') ?? '').split(',').filter(Boolean))
const sleep = (ms: number) => new Promise(r => setTimeout(r, ms))
const INBOX = 1

const DEVICES: DeviceInfo[] = [
  { name: 'fake_mic', description: 'Microfone interno (fake)', is_monitor: false, is_default: true },
  { name: 'fake_mic_silent', description: 'Microfone mudo (fake)', is_monitor: false, is_default: false },
  { name: 'fake_mic_flaky', description: 'Microfone instável (fake)', is_monitor: false, is_default: false },
  { name: 'fake_sink.monitor', description: 'Monitor da saída padrão (fake)', is_monitor: true, is_default: true },
  { name: 'fake_sink_2.monitor', description: 'Monitor do fone USB (fake)', is_monitor: true, is_default: false },
]
const bad = (code: string, detail: string) => ({ code, detail })

interface Track { dev: DeviceInfo; t0: number; silentSince: number | null; cuts: number }
interface Active { key: string; started_at: string; startMs: number; intent: Intent; mic: Track | null; sys: Track | null }
interface Intent { library_id: number; client_id: number | null; title: string; expected_speakers: number | null; language: string | null }
const rec = {
  cur: null as Active | null,
  monitor: null as { mic: Track | null; sys: Track | null; startMs: number } | null,
  finalizing: [] as string[],
  barVisible: false,
  timer: undefined as ReturnType<typeof setInterval> | undefined,
  orphans: [] as Orphan[],
  lastCuts: 0,
}

const pad = (n: number) => String(n).padStart(2, '0')
const localIso = (d = new Date()) => `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`
const keyOf = (iso: string) => `call_${iso.slice(0, 10)}_${iso.slice(11).replace(/:/g, '-')}`
const saveSettings = () => { try { localStorage.setItem(SETTINGS_KEY, JSON.stringify(settings)) } catch {} }
const parseChoice = (raw: string | undefined): StreamChoice => { try { return raw ? JSON.parse(raw) : 'default' } catch { return 'default' } }

function resolveTrack(kind: 'mic' | 'sys', choice: StreamChoice | null | undefined): Track | null {
  const c = choice ?? 'default'
  if (c === 'off') return null
  const want = kind === 'sys'
  const dev = c === 'default' ? DEVICES.find(d => d.is_monitor === want && d.is_default) : DEVICES.find(d => d.name === c.named && d.is_monitor === want)
  if (!dev) throw bad('device_not_found', typeof c === 'string' ? 'default' : c.named)
  return { dev, t0: Date.now(), silentSince: null, cuts: 0 }
}

/** Nível sintético: mic = voz com pausas; monitor = rajadas; `silent` = zero; `flaky` cai entre 3 s e 4,5 s (um corte). */
function levelOf(tr: Track, now: number): StreamLevel {
  const t = (now - tr.t0) / 1000
  let peak = 0, alive = true
  if (tr.dev.name === 'fake_mic_silent') peak = 0
  else if (tr.dev.is_monitor) peak = Math.floor(t / 5) % 3 === 2 ? 0 : Math.max(0, 0.3 + 0.2 * Math.sin(t * 1.7) + (Math.random() - 0.5) * 0.12)
  else peak = Math.max(0, 0.22 + 0.12 * Math.sin(t * 2.3) * Math.sin(t * 0.7) + (Math.random() - 0.5) * 0.08)
  if (tr.dev.name === 'fake_mic_flaky' && t >= 3 && t < 4.5) { alive = false; peak = 0; if (tr.cuts === 0) tr.cuts = 1 }
  if (peak < 0.01) tr.silentSince ??= now; else tr.silentSince = null
  return { peak: Math.min(1, peak), rms: Math.min(1, peak * 0.62), silent_s: tr.silentSince ? (now - tr.silentSince) / 1000 : 0, alive }
}

const streamStatus = (tr: Track | null, now: number): StreamStatus | null =>
  tr ? { device: tr.dev.name, description: tr.dev.description, is_monitor: tr.dev.is_monitor, alive: !(tr.dev.name === 'fake_mic_flaky' && now - tr.t0 >= 3000 && now - tr.t0 < 4500), samples: Math.floor((now - tr.t0) * 16), cuts: tr.cuts } : null

function recInfo(): RecordingInfo | null {
  const c = rec.cur
  if (!c) return null
  const now = Date.now()
  const mic = streamStatus(c.mic, now), sys = streamStatus(c.sys, now)
  return { key: c.key, started_at: c.started_at, ...c.intent, elapsed_s: (now - c.startMs) / 1000, mic, sys, cuts: (mic?.cuts ?? 0) + (sys?.cuts ?? 0) }
}
const recStatus = (): RecordStatus => ({
  state: rec.cur ? 'recording' : 'idle', app_running: true, recording: recInfo(), finalizing: [...rec.finalizing],
  bar_visible: rec.barVisible, shortcut: shortcutInfo().accelerator, shortcut_supported: shortcutInfo().supported,
})
const pushState = () => void emit('record-state', recStatus())

function tick() {
  const now = Date.now()
  const src = rec.cur ? { source: 'recording' as const, mic: rec.cur.mic, sys: rec.cur.sys, start: rec.cur.startMs }
    : rec.monitor ? { source: 'monitor' as const, mic: rec.monitor.mic, sys: rec.monitor.sys, start: null } : null
  if (!src) { clearInterval(rec.timer); rec.timer = undefined; return }
  const ev: LevelsEvent = {
    source: src.source, elapsed_s: src.start == null ? null : (now - src.start) / 1000,
    mic: src.mic ? levelOf(src.mic, now) : null, sys: src.sys ? levelOf(src.sys, now) : null,
  }
  void emit('record-levels', ev)
  // corte/reconexão muda o estado (§9): o shell reemite `record-state` com `cuts` atualizado
  const cuts = (rec.cur?.mic?.cuts ?? 0) + (rec.cur?.sys?.cuts ?? 0)
  if (rec.cur && cuts !== rec.lastCuts) { rec.lastCuts = cuts; pushState() }
}
const ensureTicker = () => { rec.timer ??= setInterval(tick, 100) }

// ---- configuração / atalho
const wayland = flags.has('wayland') || flags.has('gnome')
const shortcutDefault = 'Ctrl+Alt+R'
function shortcutInfo() {
  const raw = settings.record_shortcut
  const accelerator = wayland ? null : raw === 'none' ? null : raw ?? shortcutDefault
  return { accelerator, supported: !wayland, registered: !wayland && accelerator !== null, error: null as string | null }
}
const lastUsed = () => {
  const lib = Number(settings.record_library_id)
  const libOk = libs.some(l => l.id === lib)
  const cli = Number(settings.record_client_id)
  return {
    library_id: libOk ? lib : null,
    client_id: libOk && clients.some(c => c.id === cli && c.library_id === lib) ? cli : null,
    mic: parseChoice(settings.record_mic), sys: parseChoice(settings.record_sys),
  }
}

/** `Intent::resolve` (§4.4): alvo ausente = inbox; cliente de outra biblioteca = invalid; falantes 1..=20. */
function resolveIntent(a: Args): Intent {
  const library_id = a.libraryId ?? INBOX
  const l = libs.find(x => x.id === library_id)
  if (!l || !l.available) throw bad('not_found', `library ${library_id}`)
  let client_id: number | null = a.clientId ?? null
  if (client_id != null) {
    const c = clients.find(x => x.id === client_id)
    if (!c) throw bad('not_found', `client ${client_id}`)
    if (c.library_id !== library_id) throw bad('invalid', 'client does not belong to library')
  }
  if (l.kind === 'inbox') client_id = null
  const sp = a.expectedSpeakers ?? null
  if (sp != null && !(Number.isInteger(sp) && sp >= 1 && sp <= 20)) throw bad('invalid', 'expected_speakers out of range')
  if (a.language != null && !['pt', 'en', 'es'].includes(a.language)) throw bad('invalid', 'language')
  return { library_id, client_id, title: String(a.title ?? '').trim(), expected_speakers: sp, language: a.language ?? null }
}

function startRecording(a: Args): RecordStatus {
  if (flags.has('nobackend')) throw bad('backend_unavailable', 'no audio server')
  if (rec.cur) throw bad('already_recording', rec.cur.key)
  const lu = lastUsed()
  const target = a.libraryId == null && a.clientId == null ? { libraryId: lu.library_id, clientId: lu.client_id } : { libraryId: a.libraryId, clientId: a.clientId }
  const mic = a.mic ?? lu.mic, sys = a.sys ?? lu.sys
  if (mic === 'off' && sys === 'off') throw bad('invalid', 'both tracks are off')
  const intent = resolveIntent({ ...a, ...target })
  const mt = resolveTrack('mic', mic), st = resolveTrack('sys', sys)
  rec.monitor = null // o shell derruba a pré-visualização ao gravar
  const started_at = localIso()
  let key = keyOf(started_at)
  if (rec.finalizing.includes(key) || calls.some(c => c.key === key)) key += '_2'
  const now = Date.now()
  if (mt) mt.t0 = now
  if (st) st.t0 = now
  rec.cur = { key, started_at, startMs: now, intent, mic: mt, sys: st }
  rec.lastCuts = 0
  // save_last_used
  const set = (k: string, v: string | null) => { if (v == null) delete settings[k]; else settings[k] = v }
  set('record_library_id', String(intent.library_id)); set('record_client_id', intent.client_id == null ? null : String(intent.client_id))
  set('record_mic', JSON.stringify(mic)); set('record_sys', JSON.stringify(sys)); saveSettings()
  rec.barVisible = settings.record_bar_on_start !== '0'
  ensureTicker()
  pushState()
  return recStatus()
}

async function finalizeRun(key: string, intent: Intent | null, started_at: string, duration_s: number, tracks: ('mic' | 'sys')[], repair: boolean) {
  if (!rec.finalizing.includes(key)) rec.finalizing.push(key)
  pushState()
  const fail = async (e: { code: string; detail: string }) => {
    rec.finalizing = rec.finalizing.filter(k => k !== key)
    await emit('record-finalize-done', { key, ok: false, call: null, error: e })
    pushState()
  }
  if (duration_s < 0.5) { await sleep(300); return fail(bad('empty_recording', key)) }
  for (const track of tracks) {
    if (repair) { await emit('record-finalize-progress', { key, stage: 'repair', track }); await sleep(350) }
    for (let i = 0; i <= 10; i++) { await emit('record-finalize-progress', { key, stage: 'convert', track, done: i, of: 10 }); await sleep(90) }
  }
  await emit('record-finalize-progress', { key, stage: 'call' })
  await sleep(250)
  const lib = intent && libs.some(l => l.id === intent.library_id && l.available) ? intent.library_id : INBOX
  const clientOk = intent?.client_id != null && clients.some(c => c.id === intent.client_id && c.library_id === lib)
  let ckey = key
  if (calls.some(c => c.library_id === lib && c.key === ckey)) return fail(bad('conflict', ckey))
  const call = mkPending(lib, ++seq, ckey, intent?.title ?? '', clientOk ? intent!.client_id : null, Math.round(duration_s), { expected_speakers: intent?.expected_speakers, language: intent?.language })
  call.started_at = started_at
  calls.push(call)
  rec.finalizing = rec.finalizing.filter(k => k !== key)
  await emit('record-finalize-done', { key, ok: true, call: { library_id: lib, call_id: call.id, key: ckey }, error: null })
  await emit('data-changed', { event: 'changed', library_id: lib, call_id: call.id })
  pushState()
}

const emitOrphans = () => void emit('record-recovery', structuredClone(rec.orphans))
function seedOrphans() {
  const mk = (key: string, state: Orphan['state'], dur: number, intent: Orphan['intent'], mic: string | null, sys: string | null): Orphan =>
    ({ key, state, started_at: `${key.slice(5, 15)}T${key.slice(16).replace(/-/g, ':')}`, duration_s: dur, size_bytes: Math.round(dur * 32000 * ((mic ? 1 : 0) + (sys ? 1 : 0))), mic_device: mic, sys_device: sys, intent })
  rec.orphans = [
    mk('call_2026-09-30_14-02-11', 'recording', 1830, { library_id: 2, client_id: 1, title: 'Reunião de alinhamento', expected_speakers: 4, language: 'pt' }, 'fake_mic', 'fake_sink.monitor'),
    mk('call_2026-09-29_09-30-00', 'complete', 412, null, 'fake_mic', null),
  ]
}

Object.assign(handlers, {
  record_info: () => {
    if (flags.has('noinfo')) throw bad('not_implemented', 'record_info')
    return {
      backend: flags.has('nobackend') ? 'unavailable' : 'fake', session_type: wayland ? 'wayland' : 'x11',
      desktop: flags.has('gnome') ? 'GNOME' : wayland ? 'Hyprland' : 'X-Cinnamon',
      shortcut: shortcutInfo(), shortcut_default: shortcutDefault, bar_on_start: settings.record_bar_on_start !== '0', last_used: lastUsed(),
    }
  },
  record_devices: () => flags.has('nobackend') ? { backend: 'unavailable', devices: [] } : { backend: 'fake', devices: DEVICES },
  record_status: () => recStatus(),
  record_start: (a: Args) => startRecording(a),
  record_update: (a: Args) => {
    if (!rec.cur) throw bad('not_recording', 'no active recording')
    rec.cur.intent = resolveIntent(a) // substituição total (§4.4)
    pushState()
    return recStatus()
  },
  record_stop: () => {
    const c = rec.cur
    if (!c) throw bad('not_recording', 'no active recording')
    const dur = (Date.now() - c.startMs) / 1000
    rec.cur = null
    void finalizeRun(c.key, c.intent, c.started_at, dur, [c.mic && 'mic', c.sys && 'sys'].filter(Boolean) as ('mic' | 'sys')[], false)
    return recStatus()
  },
  record_toggle: () => (rec.cur ? handlers.record_stop({}) : startRecording({})),
  record_monitor_start: (a: Args) => {
    if (flags.has('nobackend')) throw bad('backend_unavailable', 'no audio server')
    if (rec.cur) throw bad('already_recording', rec.cur.key)
    const mic = resolveTrack('mic', a.mic), sys = resolveTrack('sys', a.sys)
    rec.monitor = { mic, sys, startMs: Date.now() }
    ensureTicker()
    return null
  },
  record_monitor_stop: () => { rec.monitor = null; return null },
  record_orphans: () => structuredClone(rec.orphans),
  record_recover: (a: Args) => {
    const o = rec.orphans.find(x => x.key === a.key)
    if (!o) throw bad('not_found', a.key)
    if (rec.cur?.key === a.key) throw bad('invalid', 'active recording')
    void finalizeRun(o.key, o.intent, o.started_at, o.duration_s, [o.mic_device && 'mic', o.sys_device && 'sys'].filter(Boolean) as ('mic' | 'sys')[], o.state === 'recording').then(() => {
      rec.orphans = rec.orphans.filter(x => x.key !== a.key)
      emitOrphans()
    })
    return null
  },
  record_discard: (a: Args) => {
    if (!rec.orphans.some(x => x.key === a.key)) throw bad('not_found', a.key)
    rec.orphans = rec.orphans.filter(x => x.key !== a.key)
    setTimeout(emitOrphans, 50)
    return null
  },
  record_set_shortcut: (a: Args) => {
    if (a.accelerator != null && !/^((Ctrl|Alt|Shift|Super|CmdOrCtrl|CommandOrControl)\+)+[A-Za-z0-9]+$/.test(a.accelerator)) throw bad('invalid', `bad accelerator: ${a.accelerator}`)
    if (wayland) return shortcutInfo()
    settings.record_shortcut = a.accelerator ?? 'none'
    saveSettings()
    pushState()
    return shortcutInfo()
  },
  bar_show: () => { rec.barVisible = true; pushState(); return null },
  bar_hide: () => { rec.barVisible = false; pushState(); return null },
  show_main_window: () => null,
  // ---- transcrição: ver o bloco "transcrição (fase 4)" abaixo
})

// ---------------------------------------------------------------- transcrição (fase 4)
// Simulação temporizada fiel ao TRANSCRIPTION_CONTRACT §4/§5/§9: fila (um job por vez), etapas com progresso em rajadas de
// 30 s no ASR e 1ª fase indeterminada na diarização, pausa por gravação/usuário, falha, instalação com download retomável.
// Flags na URL (o idioma recarrega a página, então vão na query): ?runtime=missing|outdated · ?paused=recording|user · ?backlog=N
// · ?fail[=código]
// · ?hold=<etapa> (congela a tarefa nessa etapa, para capturas) · ?fake (modo de teste) · ?auto=0 (fila não automática).
const qp = new URLSearchParams(location.search)
const txFlag = {
  missing: qp.get('runtime') === 'missing', outdated: qp.get('runtime') === 'outdated',
  forcePause: qp.get('paused') === 'recording' ? 'recording' : qp.get('paused') === 'user' ? 'user' : null,
  fail: qp.has('fail') ? qp.get('fail') || 'audio_decode' : null, hold: qp.get('hold') as JobStage | null,
  fake: qp.has('fake'), manual: qp.get('auto') === '0',
}
function effSettings() { return txFlag.manual ? { ...settings, transcription_auto: '0' } : { ...settings } }

const MODEL_FILES: Record<ModelStatus['id'], string> = {
  whisper: 'model.bin', segmentation: 'segmentation-3-0.tar.bz2', embedding: 'campplus_zh_en_16k.onnx',
}
const models: ModelStatus[] = [
  { id: 'whisper', installed: !txFlag.missing, bytes_total: 1_621_665_983, bytes_done: txFlag.missing ? 0 : 1_621_665_983, local: false },
  { id: 'segmentation', installed: !txFlag.missing, bytes_total: 6_935_020, bytes_done: txFlag.missing ? 0 : 6_935_020, local: false },
  { id: 'embedding', installed: !txFlag.missing, bytes_total: 28_300_000, bytes_done: txFlag.missing ? 0 : 28_300_000, local: false },
]
let runtimeReady = !txFlag.missing && !txFlag.outdated
const jobs: JobInfo[] = []
let jobSeq = 0
const bleedBy = new Map<number, BleedRemoval[]>() // por id da versão (único no mock)

// ---- instalação (retomável: o que já veio fica em bytes_done)
let setupRun: { cancel: boolean; phase: 'runtime' | 'models' } | null = null
async function runSetup() {
  const me: NonNullable<typeof setupRun> = (setupRun = { cancel: false, phase: 'runtime' })
  const ev = (o: Partial<SetupEv>) => emit('transcription-setup', { phase: 'runtime', step: null, index: null, of: null, model: null, file: null, bytes_done: null, bytes_total: null, error: null, ...o })
  pushQueue() // as tarefas passam a `runtime_installing`
  try {
    if (!runtimeReady) {
      const steps = ['download_uv', 'install_python', 'create_venv', 'sync_packages', 'verify'] as const
      for (let i = 0; i < steps.length; i++) {
        if (me.cancel) throw 0
        await ev({ phase: 'runtime', step: steps[i], index: i + 1, of: steps.length })
        await sleep(steps[i] === 'sync_packages' ? 1500 : 700)
      }
      runtimeReady = true
    }
    me.phase = 'models'
    pushQueue()
    const todo = models.filter(m => !m.installed)
    for (let i = 0; i < todo.length; i++) {
      const m = todo[i]
      const chunk = Math.ceil(m.bytes_total / (m.id === 'whisper' ? 50 : 8))
      while (m.bytes_done < m.bytes_total) {
        if (me.cancel) throw 0
        m.bytes_done = Math.min(m.bytes_total, m.bytes_done + chunk)
        await ev({ phase: 'models', index: i + 1, of: todo.length, model: m.id, file: MODEL_FILES[m.id], bytes_done: m.bytes_done, bytes_total: m.bytes_total })
        await sleep(110)
      }
      m.installed = true
    }
    setupRun = null
    pushQueue()
    await ev({ phase: 'finished' })
  } catch {
    setupRun = null
    pushQueue()
    await ev({ phase: 'finished', error: { code: 'setup_cancelled', detail: '' } })
  }
}
type SetupEv = { phase: string; step: string | null; index: number | null; of: number | null; model: string | null; file: string | null; bytes_done: number | null; bytes_total: number | null; error: { code: string; detail: string } | null }

/** Motor instalado de uma época anterior (`?runtime=outdated`): o que o manifest guarda difere do esperado. */
function runtimeStatus() {
  const state = txFlag.fake ? 'fake' : runtimeReady ? 'ready' : txFlag.outdated ? 'outdated' : 'missing'
  const installed = txFlag.outdated || runtimeReady
  const differences = state === 'outdated'
    ? [{ field: 'python', installed: '3.12.8', expected: '3.12.15' }, { field: 'lock', installed: '4c1e9a7b02d5', expected: 'b83f50e6a1c9' }, { field: 'worker', installed: '91d0c47e3a2f', expected: 'e5a7720b6c18' }]
    : []
  return { state, runtime_version: 1, python: '3.12.15', uv: '0.12.22', installed_at: installed ? (txFlag.outdated && !runtimeReady ? '2026-09-12T10:03:41' : now()) : null, differences }
}

const txReady = () => txFlag.fake || (runtimeReady && models.every(m => m.installed))
function txStatus() {
  return {
    runtime: runtimeStatus(),
    models: structuredClone(models), setup: { running: !!setupRun, phase: setupRun?.phase ?? null }, queue: queueStatus(), fake_worker: txFlag.fake,
  }
}

// ---- fila
const pausedReason = (): PauseReason | null => (settings.transcription_queue_paused === '1' || txFlag.forcePause === 'user' ? 'user' : rec.cur || txFlag.forcePause === 'recording' ? 'recording' : null)
/** Mesma precedência do núcleo (`Gate::blocker`): pausa → instalação → motor ausente → desatualizado → modelos. */
function queueBlocker(): BlockedBy | null {
  const paused = pausedReason()
  if (paused) return paused === 'user' ? 'paused_user' : 'paused_recording'
  const rt = runtimeStatus().state
  if (rt !== 'ready' && rt !== 'fake') return setupRun?.phase === 'runtime' ? 'runtime_installing' : rt === 'outdated' ? 'runtime_outdated' : 'runtime_missing'
  return txFlag.fake || models.every(m => m.installed) ? null : 'models_missing'
}
function queueStatus(): QueueStatus {
  const running = jobs.filter(j => j.state === 'running')
  const queued = jobs.filter(j => j.state === 'queued').sort((a, b) => a.id - b.id)
  const blocker = queueBlocker()
  for (const j of jobs) { j.blocked_by = null; j.ahead = null }
  queued.forEach((j, i) => { j.ahead = running.length + i; j.blocked_by = blocker ?? (j.ahead > 0 ? 'behind' : null) })
  const done = jobs.filter(j => !['running', 'queued'].includes(j.state)).sort((a, b) => (b.finished_at ?? '').localeCompare(a.finished_at ?? '') || b.id - a.id).slice(0, 20)
  return { paused: pausedReason(), blocked_by: blocker, jobs: structuredClone([...running, ...queued, ...done]) }
}
const pushQueue = () => void emit('queue-changed', queueStatus())
const openJob = (c: Call) => jobs.find(j => j.library_id === c.library_id && j.call_id === c.id && (j.state === 'queued' || j.state === 'running'))
const activeInfo = (c: Call) => c.transcripts.find(v => v.id === c.transcript_id)

function enqueue(libraryId: number, callId: number, kind: JobKind = 'full', options: JobOptions | null = null): JobInfo {
  const c = findCall(libraryId, callId)
  if (c.audio.deleted_at) throw bad('audio_deleted', c.key)
  if (!c.has_audio || (!c.audio.mic_path && !c.audio.sys_path)) throw bad('no_audio', c.key)
  if (openJob(c)) throw bad('conflict', 'call already has an open job')
  if (kind !== 'full' && !activeInfo(c)?.has_raw) throw bad('no_raw_data', c.key)
  const j: JobInfo = {
    id: ++jobSeq, library_id: libraryId, call_id: callId, call_key: c.key, kind, state: 'queued',
    options: { language: null, expected_speakers: null, bleed_filter: null, bleed_margin_db: null, diarization_threshold: null, ...(options ?? {}) },
    base_job_id: kind === 'full' ? null : 1, attempts: 0, stage: null, progress: null, error_code: null, error_detail: null,
    created_at: now(), started_at: null, finished_at: null, blocked_by: null, ahead: null,
  }
  jobs.push(j)
  if (c.transcript_id == null && c.transcription_state === 'failed') { c.transcription_state = 'pending'; c.transcription_error = null }
  pushQueue()
  return j
}

// ---- execução de uma tarefa
let cur: { id: number; abort: 'pause' | 'cancel' | null } | null = null
const failedOnce = new Set<string>()

const SAMPLE = [
  'Bom dia a todos, vamos começar pelo resumo da semana.', 'O serviço de notificações ficou estável depois do ajuste.',
  'Eu revisei o relatório e encontrei dois pontos para corrigir.', 'A fila de pagamentos precisa de mais um teste antes da entrega.',
  'Combinado, eu envio o resumo ainda hoje.', 'Alguém sabe se o ambiente de testes já foi atualizado?',
  'Sim, atualizamos ontem à noite, sem problemas.', 'Então fechamos o plano e marcamos a próxima conversa.',
]
const ECHO = ['o serviço de notificações ficou estável depois do ajuste', 'a fila de pagamentos precisa de mais um teste antes da entrega', 'então fechamos o plano e marcamos a próxima conversa']

/** Versão nova com blocos `Eu`/`Pessoa N` (rótulos canônicos), nomes dos falantes preservados, removidos como eco registrados. */
function commitVersion(c: Call, j: JobInfo) {
  const prev = c.transcript_id != null ? { speakers: c.speakers, blocks: c.blocks } : null
  const prevPeople = prev ? prev.speakers.filter(s => s.track === 'sys').length : 0
  const n = j.kind === 'resegment' && prevPeople ? prevPeople : Math.min(20, Math.max(1, j.options.expected_speakers ?? c.expected_speakers ?? 2))
  const bleedOn = j.options.bleed_filter !== false && settings.bleed_filter !== '0'
  const names = new Map(prev?.speakers.map(s => [s.label, s.name]))
  const speakers: SpeakerInfo[] = [{ id: ++seq, track: 'mic', label: 'Eu', name: names.get('Eu') ?? null }]
  for (let i = 1; i <= n; i++) speakers.push({ id: ++seq, track: 'sys', label: `Pessoa ${i}`, name: names.get(`Pessoa ${i}`) ?? null })
  const count = Math.min(30, Math.max(6, Math.round(c.duration_s / 25)))
  const lines: { t: number; spk: SpeakerInfo; text: string; echo?: boolean }[] = []
  for (let i = 0; i < count; i++) {
    const t0 = Math.round((i * c.duration_s) / count)
    lines.push({ t: t0, spk: i % 3 === 2 ? speakers[0] : speakers[1 + (i % n)], text: SAMPLE[i % SAMPLE.length] })
  }
  const removals: BleedRemoval[] = ECHO.map((text, i) => ({
    id: i + 1, t_start: 40 + i * 70, t_end: 44 + i * 70, text, containment: 0.8 + i * 0.05, margin_db: i === 2 ? null : -22.1 - i * 3.4,
    reason: i === 2 ? 'energy_short' : 'text_and_energy',
  }))
  // sem o filtro de eco, esses trechos do microfone ficam no texto
  if (!bleedOn) removals.forEach(r => lines.push({ t: r.t_start, spk: speakers[0], text: r.text[0].toUpperCase() + r.text.slice(1) + '.', echo: true }))
  lines.sort((a, b) => a.t - b.t)
  const blocks: BlockInfo[] = lines.map((l, i) => ({ id: ++seq, seq: i + 1, t_start: l.t, t_end: l.t + 20, speaker_id: l.spk.id, text: l.text, original_text: l.text, edited: false, deleted_at: null }))
  const id = ++seq
  bleedBy.set(id, bleedOn ? removals : [])
  if (prev) c.other = { ...(c.other ?? {}), [c.transcript_id!]: prev }
  c.transcripts.forEach(v => (v.is_active = false))
  c.transcripts.push({ id, version: c.transcripts.length + 1, model: 'large-v3-turbo', engine: 'faster-whisper', source_file: null, created_at: now(), is_active: true, has_raw: true })
  Object.assign(c, { transcript_id: id, speakers, blocks, versions: c.transcripts.length, transcription_state: 'done', transcription_error: null })
  return id
}

const sleepAbortable = async (ms: number) => { const end = Date.now() + ms; while (Date.now() < end && !cur!.abort) await sleep(Math.min(100, end - Date.now())) }

async function runJob(j: JobInfo) {
  const c = findCall(j.library_id, j.call_id)
  const me = (cur = { id: j.id, abort: null })
  const total = c.duration_s
  const stages: JobStage[] = j.kind === 'full' ? ['preparing', 'loading_model', 'asr_sys', 'asr_mic', 'energy', 'diarize', 'assemble', 'commit']
    : j.kind === 'rediarize' ? ['preparing', 'diarize', 'assemble', 'commit'] : ['preparing', 'assemble', 'commit']
  Object.assign(j, { state: 'running', attempts: j.attempts + 1, started_at: now(), stage: 'preparing', progress: null })
  if (c.transcript_id == null) c.transcription_state = 'running'
  pushQueue()
  const prog = (stage: JobStage, fraction: number | null, audio_s: number | null = null) => {
    j.stage = stage; j.progress = fraction
    const p: JobProgress = { job_id: j.id, library_id: j.library_id, call_id: j.call_id, stage, fraction, audio_s, total_s: audio_s == null ? null : total }
    void emit('queue-progress', p)
  }
  for (const stage of stages) {
    if (me.abort) break
    j.stage = stage; j.progress = null
    pushQueue()
    if (txFlag.hold === stage) { // congela para capturas: ASR em 42 %, as demais etapas sem fração
      prog(stage, stage.startsWith('asr') ? 0.42 : null, stage.startsWith('asr') ? total * 0.42 : null)
      while (!me.abort) await sleep(150)
      break
    }
    if (stage === 'preparing') await sleepAbortable(600)
    else if (stage === 'loading_model') { prog(stage, null); await sleepAbortable(1200) }
    else if (stage === 'asr_sys' || stage === 'asr_mic') {
      // o whisper devolve o progresso em rajadas de 30 s de áudio
      for (let a = 0; a < total && !me.abort; ) {
        a = Math.min(total, a + 30)
        prog(stage, a / total, a)
        if (txFlag.fail && stage === 'asr_sys' && !failedOnce.has(c.key) && a >= Math.min(total, 30)) {
          failedOnce.add(c.key)
          Object.assign(j, { state: 'failed', error_code: txFlag.fail, error_detail: 'synthetic failure (mock): could not decode audio stream', finished_at: now() })
          if (c.transcript_id == null) Object.assign(c, { transcription_state: 'failed', transcription_error: j.error_detail })
          cur = null; pushQueue(); return
        }
        await sleepAbortable(300)
      }
    } else if (stage === 'energy') { for (let i = 1; i <= 5 && !me.abort; i++) { prog(stage, i / 5); await sleepAbortable(160) } }
    else if (stage === 'diarize') {
      prog(stage, null) // 1ª fase (~40 %) sem progresso
      await sleepAbortable(1800)
      for (let i = 1; i <= 8 && !me.abort; i++) { prog(stage, i / 8); await sleepAbortable(160) }
      await sleepAbortable(1200) // depois da segmentação (8/8) não há mais progresso até o resultado
    } else await sleepAbortable(450)
  }
  cur = null
  if (me.abort === 'cancel') {
    Object.assign(j, { state: 'cancelled', finished_at: now(), stage: null, progress: null })
    if (c.transcript_id == null) c.transcription_state = 'pending'
  } else if (me.abort === 'pause') { // volta à fila com o bruto preservado e segue sozinha depois
    Object.assign(j, { state: 'queued', stage: null, progress: null })
    if (c.transcript_id == null) c.transcription_state = 'pending'
  } else {
    const tid = commitVersion(c, j)
    Object.assign(j, { state: 'done', finished_at: now(), stage: null, progress: null })
    pushQueue()
    await emit('data-changed', { event: 'transcribed', library_id: j.library_id, call_id: j.call_id, job_id: j.id, transcript_id: tid })
    await emit('data-changed', { event: 'changed', library_id: j.library_id, call_id: j.call_id })
    return
  }
  pushQueue()
}

let lastPause: PauseReason | null = null
function txTick() {
  const pause = pausedReason()
  if (pause !== lastPause) { lastPause = pause; pushQueue() }
  if (pause && cur) cur.abort ??= 'pause'
  // `transcription_auto`: pega as chamadas pendentes sozinha (as com falha não voltam sozinhas)
  if ((effSettings().transcription_auto ?? '1') === '1') {
    for (const c of calls) if (c.transcript_id == null && c.transcription_state === 'pending' && c.has_audio && !openJob(c)) { try { enqueue(c.library_id, c.id) } catch { /* sem áudio */ } }
  }
  if (cur || pause || !txReady()) return
  const next = jobs.filter(j => j.state === 'queued').sort((a, b) => a.id - b.id)[0]
  if (next) void runJob(next)
}

// uma chamada já transcrita com bruto (Eu / Pessoa N) para testar separar vozes/remontar/eco sem esperar a fila
{
  const demo = mkCall(2, 5, 'call_2026-03-03_14-00-00', 'Retrospectiva (demo)', 1, [
    [3, 'Pessoa 1', 'Bom dia a todos, vamos começar pelo resumo da semana.'],
    [18, 'Eu', 'Bom dia. Eu revisei o relatório e encontrei dois pontos para corrigir.'],
    [41, 'Pessoa 2', 'O serviço de notificações ficou estável depois do ajuste.'],
    [66, 'Pessoa 1', 'A fila de pagamentos precisa de mais um teste antes da entrega.'],
    [92, 'Pessoa 3', 'Alguém sabe se o ambiente de testes já foi atualizado?'],
    [110, 'Eu', 'Combinado, eu envio o resumo ainda hoje.'],
    [131, 'Pessoa 2', 'Sim, atualizamos ontem à noite, sem problemas.'],
  ])
  demo.expected_speakers = 3
  demo.transcripts[0].id = demo.transcript_id = ++seq
  demo.transcripts[0].has_raw = true
  demo.transcripts[0].source_file = null
  // 2ª versão: o cabeçalho da chamada fica no pior caso (seletor de versão + Falantes + Eco + Refazer)
  addVersion(demo, 'large-v3', [[3, 'Pessoa 1', 'Bom dia a todos (segunda versão).'], [18, 'Eu', 'Bom dia.']])
  calls.push(demo)
  bleedBy.set(demo.transcript_id, ECHO.map((text, i) => ({
    id: i + 1, t_start: 40 + i * 70, t_end: 44 + i * 70, text, containment: 0.8 + i * 0.05, margin_db: i === 2 ? null : -22.1 - i * 3.4,
    reason: (i === 2 ? 'energy_short' : 'text_and_energy') as BleedRemoval['reason'],
  })))
}
// ---- apagar empresa/cliente (mesmas regras de `removal.rs`: recusas antes de mexer, modo obrigatório com chamadas)
function deletion(a: Args, clientId: number | null): Deletion | Promise<Deletion> {
  const l = lib(a.libraryId)
  if (l.kind === 'inbox') throw bad('invalid', clientId == null ? 'the inbox (unclassified calls) cannot be deleted' : 'unclassified calls have no clients')
  const cl = clientId == null ? null : clients.find(c => c.id === clientId && c.library_id === l.id)
  if (clientId != null && !cl) throw bad('not_found', `client ${clientId}`)
  const mine = calls.filter(c => c.library_id === l.id && (clientId == null || c.client_id === clientId))
  const busy = mine.find(openJob)
  if (busy) throw bad('conflict', `call ${busy.key} has a transcription job queued or running`)
  const target = rec.cur?.intent
  if (target && target.library_id === l.id && (clientId == null || target.client_id === clientId)) throw bad('conflict', 'a recording to this destination is in progress or being finalized')
  const mode: DeleteMode | null = a.mode ?? null
  const glossary = rules.filter(r => r.scope === 'client' && r.library_id === l.id && (clientId == null || r.client_id === clientId)).length
  const keepBlocked = clientId == null && mine.some(c => calls.some(x => x.library_id === 1 && x.key === c.key)) ? 'call already exists in the inbox' : null
  const out: Deletion = {
    dry_run: !!a.dryRun, library_id: l.id, client_id: clientId, name: cl?.name ?? l.name, mode, calls: mine.length,
    audio_bytes: mine.reduce((n, c) => n + (c.audio.deleted_at ? 0 : audioBytesOf(c)), 0),
    clients: clientId == null ? clients.filter(c => c.library_id === l.id).length : 0, glossary_entries: glossary,
    keep_blocked: keepBlocked, moved: 0, deleted: 0, folder_removed: false, leftover: [],
  }
  if (a.dryRun) return out
  if (mine.length && !mode) throw bad('invalid', `${mine.length} call(s) would be affected: choose keep or delete`)
  if (mode === 'keep' && keepBlocked) throw bad('conflict', keepBlocked)
  return new Promise(resolve => setTimeout(() => { // mover pastas pode demorar: dá tempo de ver o estado "ocupado"
    for (const c of mine) {
      if (mode === 'keep') { if (clientId == null) c.library_id = 1; c.client_id = null; out.moved++ }
      else { calls.splice(calls.indexOf(c), 1); jobs.splice(0, jobs.length, ...jobs.filter(j => !(j.library_id === c.library_id && j.call_id === c.id))); out.deleted++ }
    }
    for (let i = rules.length - 1; i >= 0; i--) {
      const r = rules[i]
      if (r.scope === 'client' && r.library_id === l.id && (clientId == null || r.client_id === clientId)) rules.splice(i, 1)
      else if (clientId == null && r.scope === 'global' && r.source_library_id === l.id) Object.assign(r, { source_edit_id: null, source_library_id: null })
    }
    if (clientId == null) {
      clients.splice(0, clients.length, ...clients.filter(c => c.library_id !== l.id))
      jobs.splice(0, jobs.length, ...jobs.filter(j => j.library_id !== l.id)) // como `DELETE FROM transcription_jobs WHERE library_id`
      libs.splice(libs.indexOf(l), 1)
      out.folder_removed = true
    } else clients.splice(clients.indexOf(cl!), 1)
    // `record_*`/`last_*` que apontavam para o que sumiu (os ids são reaproveitados)
    for (const [lk, ck] of [['record_library_id', 'record_client_id'], ['last_library_id', 'last_client_id']]) {
      if (Number(settings[lk]) !== l.id) continue
      if (clientId == null) { delete settings[lk]; delete settings[ck] } else if (Number(settings[ck]) === clientId) delete settings[ck]
    }
    try { localStorage.setItem(SETTINGS_KEY, JSON.stringify(settings)) } catch {}
    resolve(out)
  }, 700))
}
Object.assign(handlers, {
  delete_library: (a: Args) => deletion(a, null),
  delete_client: (a: Args) => deletion(a, a.clientId),
})

// ---- áudio das chamadas (#24; mesmas regras de `storage::delete_audio`)
/** ~28 KB/s: as duas trilhas em FLAC, mais ~1% de cache de picos */
const audioBytesOf = (c: Call) => Math.round(c.duration_s * 28_000)
function audioPlan(c: Call, dryRun: boolean): AudioDeletion {
  const gone = !!c.audio.deleted_at
  if (rec.cur?.key === c.key) throw bad('conflict', `call ${c.key} is being recorded`)
  if (openJob(c)) throw bad('conflict', `call ${c.key} has a transcription job queued or running`)
  if (!gone && c.transcript_id == null) throw bad('not_transcribed', c.key)
  const bytes = gone ? 0 : audioBytesOf(c)
  const peaks = Math.round(bytes * 0.01)
  const files: AudioDeletion['files'] = gone ? [] : [
    { name: 'mic.flac', bytes: Math.round((bytes - peaks) * 0.4), kind: 'audio' },
    { name: 'sys.flac', bytes: Math.round((bytes - peaks) * 0.6), kind: 'audio' },
    { name: 'peaks.bin', bytes: peaks, kind: 'cache' },
  ]
  if (!dryRun && !gone) Object.assign(c, { has_audio: false, audio: { ...c.audio, deleted_at: now() } })
  return { library_id: c.library_id, call_id: c.id, call_key: c.key, dry_run: dryRun, already_deleted: gone, files, bytes, deleted_at: c.audio.deleted_at }
}
Object.assign(handlers, {
  audio_list: () => {
    const list: AudioEntry[] = calls.filter(c => c.has_audio && !c.audio.deleted_at).map(c => ({
      library_id: c.library_id, call_id: c.id, call_key: c.key, title: c.title,
      client_name: clients.find(x => x.id === c.client_id)?.name ?? null, started_at: c.started_at, duration_s: c.duration_s,
      bytes: audioBytesOf(c), blocked: c.transcript_id == null ? 'not_transcribed' as const : openJob(c) ? 'job_open' as const : null,
    })).sort((a, b) => b.bytes - a.bytes)
    return { total_bytes: list.reduce((n, e) => n + e.bytes, 0), calls: list }
  },
  audio_delete: (a: Args) => audioPlan(findCall(a.libraryId, a.callId), !!a.dryRun),
  transcription_status: () => txStatus(),
  transcription_setup_start: () => {
    if (setupRun) throw bad('conflict', 'setup already running')
    void runSetup()
    return null
  },
  transcription_setup_cancel: () => { if (setupRun) setupRun.cancel = true; return null },
  models_import_local: (a: Args) => {
    const m = models.find(x => x.id === a.model)
    if (!m) throw bad('invalid', `unknown model ${a.model}`)
    if (!String(a.path ?? '').trim()) throw bad('not_found', 'path')
    Object.assign(m, { installed: true, bytes_done: m.bytes_total, local: true })
    return structuredClone(models)
  },
  transcribe_enqueue: (a: Args) => enqueue(a.libraryId, a.callId, a.kind ?? 'full', a.options ?? null),
  transcribe_pending: () => {
    const out: JobInfo[] = []
    for (const c of calls) if (c.transcript_id == null && c.transcription_state === 'pending' && !openJob(c)) { try { out.push(enqueue(c.library_id, c.id)) } catch { /* sem áudio */ } }
    return out
  },
  queue_status: () => queueStatus(),
  queue_cancel: (a: Args) => {
    const j = jobs.find(x => x.id === a.jobId)
    if (!j) throw bad('not_found', `job ${a.jobId}`)
    if (j.state === 'running') { if (cur?.id === j.id) cur.abort = 'cancel' }
    else if (j.state === 'queued') {
      Object.assign(j, { state: 'cancelled', finished_at: now() })
      pushQueue()
    } else throw bad('conflict', 'job is not open')
    return null
  },
  queue_retry: (a: Args) => {
    const j = jobs.find(x => x.id === a.jobId)
    if (!j) throw bad('not_found', `job ${a.jobId}`)
    if (j.state === 'failed') { // mesma linha: preserva o bruto parcial
      Object.assign(j, { state: 'queued', error_code: null, error_detail: null, finished_at: null, stage: null, progress: null })
      const c = findCall(j.library_id, j.call_id)
      if (c.transcript_id == null) Object.assign(c, { transcription_state: 'pending', transcription_error: null })
      pushQueue()
      return structuredClone(j)
    }
    if (j.state === 'cancelled') return enqueue(j.library_id, j.call_id, j.kind, j.options) // o bruto foi apagado: tarefa nova
    throw bad('conflict', 'job is not failed or cancelled')
  },
  queue_pause: (a: Args) => {
    if (a.paused) settings.transcription_queue_paused = '1'; else delete settings.transcription_queue_paused
    saveSettings()
    pushQueue()
    return queueStatus()
  },
  bleed_removals: (a: Args) => structuredClone(bleedBy.get(a.transcriptId) ?? []),
})

// ---- player de áudio (issue #22): relógio falso; o som de verdade é do Rust (PulseAudio)
const pl = { lib: 0, call: 0, dur: 0, pos: 0, playing: false, speed: 1, since: 0, ended: false, tick: 0 as ReturnType<typeof setInterval> | 0 }
const plPos = () => Math.min(pl.dur, pl.playing ? pl.pos + ((Date.now() - pl.since) / 1000) * pl.speed : pl.pos)
function plEmit() {
  if (pl.playing && plPos() >= pl.dur) { pl.pos = pl.dur; pl.playing = false; pl.ended = true }
  void emit('player-position', {
    library_id: pl.lib, call_id: pl.call, state: pl.playing ? 'playing' : pl.ended ? 'ended' : 'paused',
    position_s: plPos(), duration_s: pl.dur, speed: pl.speed, error: null,
  })
  if (!pl.playing && pl.tick) { clearInterval(pl.tick); pl.tick = 0 }
}
/** Fixa a posição de agora antes de mudar o estado (como o motor faz com `settle`). */
const plSettle = () => { pl.pos = plPos(); pl.since = Date.now() }
const plStop = () => { if (pl.tick) clearInterval(pl.tick); pl.tick = 0; pl.playing = false; pl.lib = 0 }
/** Onda sintética: trechos de fala (envelope suave com ruído determinístico) e pausas curtas entre eles. */
function mockPeaks(dur: number, n: number): number[] {
  let x = 12345
  const rnd = () => ((x = (x * 1103515245 + 12345) & 0x7fffffff) / 0x7fffffff)
  return Array.from({ length: n }, (_, i) => {
    const sec = (i / n) * dur
    const phrase = 0.5 + 0.5 * Math.sin(sec / 3.1) * Math.sin(sec / 11.7 + 1)
    const gap = Math.sin(sec / 7.3) > 0.82 ? 0.12 : 1
    return Math.round(Math.min(255, (30 + 190 * phrase * gap) * (0.55 + 0.45 * rnd())))
  })
}
const playerHandlers: Record<string, (a: Args) => unknown> = {
  player_open: a => {
    const c = findCall(a.libraryId, a.callId)
    plStop()
    if (c.audio.deleted_at) return { available: false, reason: 'deleted', duration_s: 0 }
    if (!c.has_audio || (!c.audio.mic_path && !c.audio.sys_path)) return { available: false, reason: 'none', duration_s: 0 }
    Object.assign(pl, { lib: a.libraryId, call: a.callId, dur: c.duration_s, pos: 0, speed: 1, ended: false })
    return { available: true, reason: null, duration_s: c.duration_s }
  },
  player_peaks: async a => {
    const c = findCall(a.libraryId, a.callId)
    await sleep(flags.has('slowpeaks') ? 2500 : 120)
    return { per_s: 50, duration_s: c.duration_s, data: mockPeaks(c.duration_s, Math.min(20000, Math.max(1, a.buckets))) }
  },
  player_play: () => {
    if (!pl.lib) throw bad('invalid', 'no call is open in the player')
    if (pl.ended) { pl.pos = 0; pl.ended = false }
    plSettle(); pl.playing = true
    if (!pl.tick) pl.tick = setInterval(plEmit, 100)
    plEmit()
  },
  player_pause: () => { if (!pl.lib) throw bad('invalid', 'no call is open in the player'); plSettle(); pl.playing = false; plEmit() },
  player_seek: a => {
    if (!pl.lib) throw bad('invalid', 'no call is open in the player')
    pl.pos = Math.min(pl.dur, Math.max(0, a.seconds)); pl.since = Date.now(); pl.ended = false; plEmit()
  },
  player_speed: a => { if (!pl.lib) throw bad('invalid', 'no call is open in the player'); plSettle(); pl.speed = Math.min(2, Math.max(1, a.speed)); plEmit() },
  player_set_cuts: () => { /* o motor de verdade relê os cortes do banco; aqui não há som */ },
  player_close: a => { if (pl.lib && (a.libraryId == null || a.libraryId === pl.lib) && (a.callId == null || a.callId === pl.call)) plStop() },
}
Object.assign(handlers, playerHandlers)

const startTxSim = () => { setInterval(txTick, 400) }

if (flags.has('orphans')) seedOrphans()

export function install() {
  startTxSim()
  mockIPC(async (cmd, args) => {
    const h = handlers[cmd]
    if (!h) throw { code: 'unknown', detail: `mock: ${cmd}` }
    return structuredClone(await h((args ?? {}) as Args))
  }, { shouldMockEvents: true })
  ;(window as any).__mock = {
    calls, history, rules, rec, jobs, models,
    addOrphans: () => { seedOrphans(); emitOrphans() },
  }
  // startup real: o shell emite `record-recovery` se houver órfãs (a UI também pergunta por `record_orphans` no boot)
  if (rec.orphans.length) setTimeout(emitOrphans, 700)
}
