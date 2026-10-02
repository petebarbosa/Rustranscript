// Única porta de saída para o núcleo Rust. Fora do Tauri (navegador, testes de UI) um backend
// falso em memória responde no lugar — ver mock.ts.
import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'

export interface LibraryInfo {
  id: number; name: string; kind: 'inbox' | 'company'; path: string
  available: boolean; call_count: number; unassigned_count: number
}
export interface ClientInfo { id: number; library_id: number; name: string; slug: string; call_count: number }
export interface CallSummary {
  library_id: number; id: number; key: string; title: string
  client_id: number | null; client_name: string | null
  started_at: string; duration_s: number; words: number; preview: string
  edited_blocks: number; versions: number; has_audio: boolean
  /** gravação recém-feita: 'pending' e sem nenhuma versão (versions = 0); chamadas importadas: 'done' */
  transcription_state: TranscriptionState; transcription_error: string | null
}
export type TranscriptionState = 'pending' | 'running' | 'done' | 'failed'
export interface TranscriptInfo {
  id: number; version: number; model: string | null; engine: string | null
  source_file: string | null; created_at: string; is_active: boolean
}
export interface SpeakerInfo { id: number; track: 'mic' | 'sys'; label: string; name: string | null }
export interface BlockInfo {
  id: number; seq: number; t_start: number; t_end: number; speaker_id: number
  text: string; original_text: string; edited: boolean
}
export interface Chapter { t: number; title: string }
export interface CallDetail extends CallSummary {
  library_name: string; language: string | null; expected_speakers: number | null
  /** null = chamada sem transcrição ainda (pendente): transcripts/speakers/blocks/chapters vêm vazios */
  transcript_id: number | null; transcripts: TranscriptInfo[]; speakers: SpeakerInfo[]
  blocks: BlockInfo[]; chapters: Chapter[]
  audio: { mic_path: string | null; sys_path: string | null; deleted_at: string | null }
}
export interface HistoryEntry {
  id: number; call_id: number; entity: 'block_text' | 'block_speaker' | 'call_title' | 'speaker_name'
  entity_id: number; old_value: string | null; new_value: string | null
  origin: 'ui' | 'cli' | 'import'; at: string; undone_at: string | null
  /** lote (ex.: glossário aplicado): null = edição avulsa */
  batch_id: number | null; batch_kind: 'glossary' | null; batch_size: number | null
}
// ---- glossário (ver GLOSSARY_CONTRACT.md)
export type RuleKind = 'term' | 'replace'
export type Scope = 'global' | 'client'
export interface Rule {
  id: number; scope: Scope; library_id: number | null; client_id: number | null
  kind: RuleKind; pattern: string; replacement: string | null; case_sensitive: boolean
  created_at: string; source_edit_id: number | null; source_library_id: number | null
  /** global escondida por uma regra de cliente de mesmo tipo+padrão */
  overridden: boolean
}
export interface Hit { scope: Scope | null; rule_id: number | null; pattern: string; replacement: string; count: number }
export interface BlockChange { block_id: number; seq: number; before: string; after: string; rules: Hit[] }
export interface ApplyReport {
  call_id: number; transcript_id: number; dry_run: boolean
  blocks_changed: number; replacements: number; batch_id: number | null; changes: BlockChange[]
}
export interface ClientRef { id: number; name: string }
export interface BlockSuggestion {
  pattern: string; replacement: string
  occurrences_in_call: number
  /** cliente da chamada; null (sem cliente) => só dá para criar regra global */
  client: ClientRef | null
}
export interface BlockEdit extends BlockInfo {
  /** edit_history.id da edição (null se o texto não mudou); vai como `sourceEditId` */
  edit_id: number | null
  suggestions: BlockSuggestion[]
}
export interface GlossaryImportEntry {
  line: number; kind: RuleKind; pattern: string; replacement: string | null
  status: 'added' | 'duplicate' | 'invalid'
  reason: string | null
}
export interface GlossaryImportReport {
  dry_run: boolean; added: number; skipped: number; invalid: number; entries: GlossaryImportEntry[]
}
export interface PromptTerms { terms: string[]; estimated_tokens: number; budget_tokens: number }
export interface RuleInput {
  kind: RuleKind; pattern: string; replacement: string | null; caseSensitive: boolean
}
export interface SearchHit {
  library_id: number; call_id: number; call_key: string; call_title: string; started_at: string
  block_id: number | null; t_start: number | null; snippet: string; rank: number
}
export interface ImportItem {
  key: string; status: 'new' | 'updated' | 'unchanged' | 'skipped'; reason: string | null
  library_id: number | null; call_id: number | null; versions_added: number[]; edits_applied: number
  audio_pending: string[]; audio_converted: string[]; audio_errors: string[]
  glossary_replacements: number
}
export interface ImportReport { dry_run: boolean; items: ImportItem[] }
export type ImportProgress =
  | { stage: 'call'; index: number; total: number; key: string }
  | { stage: 'audio'; index: number; total: number; key: string; track: string; done: number; of: number }
  | { stage: 'done' }
export interface Reclaimable {
  total_bytes: number
  files: { library_id: number; call_key: string; kind: string; path: string; size: number }[]
}
export interface Bootstrap {
  data_dir: string; system_language: string; inbox_id: number
  settings: Record<string, string>; libraries: LibraryInfo[]
}
export interface ApiError { code: string; detail: string }

// ---- gravação (fase 3; ver RECORDING_CONTRACT.md)
/** 'default' = padrão do sistema; 'off' = não gravar esta trilha; { named } = dispositivo (DeviceInfo.name) */
export type StreamChoice = 'default' | 'off' | { named: string }
export interface DeviceInfo { name: string; description: string; is_monitor: boolean; is_default: boolean }
export interface RecordDevices { backend: 'pulse' | 'fake' | 'unavailable'; devices: DeviceInfo[] }
export interface StreamStatus { device: string; description: string; is_monitor: boolean; alive: boolean; samples: number; cuts: number }
export interface RecordingInfo {
  key: string; started_at: string
  library_id: number; client_id: number | null; title: string; expected_speakers: number | null; language: string | null
  elapsed_s: number; mic: StreamStatus | null; sys: StreamStatus | null; cuts: number
}
export interface RecordStatus {
  state: 'idle' | 'recording'
  /** sempre true dentro da app; false só na resposta sintética da CLI */
  app_running: boolean
  recording: RecordingInfo | null
  /** chaves em conversão/importação em segundo plano (pode haver com state 'idle') */
  finalizing: string[]
  bar_visible: boolean
  shortcut: string | null; shortcut_supported: boolean
}
export interface ShortcutInfo { accelerator: string | null; supported: boolean; registered: boolean; error: string | null }
export interface LastUsed { library_id: number | null; client_id: number | null; mic: StreamChoice; sys: StreamChoice }
export interface RecordInfo {
  backend: 'pulse' | 'fake' | 'unavailable'
  session_type: 'x11' | 'wayland' | 'unknown'
  /** XDG_CURRENT_DESKTOP (ex.: 'Hyprland') */
  desktop: string | null
  shortcut: ShortcutInfo; shortcut_default: string
  bar_on_start: boolean
  last_used: LastUsed
}
export interface StreamLevel { peak: number; rms: number; silent_s: number; alive: boolean }
/** evento 'record-levels' (~10 Hz) */
export interface LevelsEvent { source: 'recording' | 'monitor'; elapsed_s: number | null; mic: StreamLevel | null; sys: StreamLevel | null }
export interface Orphan {
  key: string; state: 'recording' | 'complete'; started_at: string
  duration_s: number; size_bytes: number
  mic_device: string | null; sys_device: string | null
  intent: { library_id: number; client_id: number | null; title: string; expected_speakers: number | null; language: string | null } | null
}
/** evento 'record-finalize-progress' */
export type FinalizeProgress =
  | { key: string; stage: 'repair'; track: 'mic' | 'sys' }
  | { key: string; stage: 'convert'; track: 'mic' | 'sys'; done: number; of: number }
  | { key: string; stage: 'call' }
/** evento 'record-finalize-done' */
export interface FinalizeDone {
  key: string; ok: boolean
  call: { library_id: number; call_id: number; key: string } | null
  error: ApiError | null
}
export interface RecordStartArgs {
  libraryId?: number | null; clientId?: number | null; title?: string | null
  expectedSpeakers?: number | null; language?: string | null
  mic?: StreamChoice; sys?: StreamChoice
}
/** nomes dos eventos emitidos pelo shell */
export const REC_EVENTS = {
  levels: 'record-levels', state: 'record-state', finalizeProgress: 'record-finalize-progress',
  finalizeDone: 'record-finalize-done', recovery: 'record-recovery',
} as const

/** Rótulo da janela: 'bar' na mini barra, 'main' na principal (no navegador comum, pelo hash '#/bar'). */
export const windowLabel: string = (() => {
  try { return (window as any).__TAURI_INTERNALS__?.metadata?.currentWindow?.label ?? 'main' } catch { return 'main' }
})()
export const isBarWindow = () => windowLabel === 'bar' || location.hash.startsWith('#/bar')

export const inTauri = '__TAURI_INTERNALS__' in window

let ready: Promise<void> = Promise.resolve()
if (!inTauri) ready = import('./mock').then(m => m.install())

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  await ready
  try {
    return await invoke<T>(cmd, args)
  } catch (e) {
    throw toError(e)
  }
}

export function toError(e: unknown): ApiError {
  if (e && typeof e === 'object' && 'code' in e) return e as ApiError
  return { code: 'unknown', detail: String(e) }
}

export const api = {
  bootstrap: () => call<Bootstrap>('bootstrap'),
  libraries: () => call<LibraryInfo[]>('libraries'),
  addLibrary: (name: string, path: string) => call<number>('add_library', { name, path }),
  renameLibrary: (libraryId: number, name: string) => call<void>('rename_library', { libraryId, name }),
  removeLibrary: (libraryId: number) => call<void>('remove_library', { libraryId }),
  clients: (libraryId: number) => call<ClientInfo[]>('clients', { libraryId }),
  addClient: (libraryId: number, name: string) => call<ClientInfo>('add_client', { libraryId, name }),
  renameClient: (libraryId: number, clientId: number, name: string) =>
    call<void>('rename_client', { libraryId, clientId, name }),
  calls: (libraryId: number | null, clientId: number | null, unassigned: boolean) =>
    call<CallSummary[]>('calls', { libraryId, clientId, unassigned }),
  callDetail: (libraryId: number, callId: number, transcriptId: number | null = null) =>
    call<CallDetail>('call_detail', { libraryId, callId, transcriptId }),
  setBlockText: (libraryId: number, blockId: number, text: string) =>
    call<BlockEdit>('set_block_text', { libraryId, blockId, text }),
  revertBlock: (libraryId: number, blockId: number) => call<BlockInfo>('revert_block', { libraryId, blockId }),
  setTitle: (libraryId: number, callId: number, title: string) =>
    call<CallSummary>('set_title', { libraryId, callId, title }),
  renameSpeaker: (libraryId: number, speakerId: number, name: string | null) =>
    call<SpeakerInfo>('rename_speaker', { libraryId, speakerId, name }),
  setBlockSpeaker: (libraryId: number, blockId: number, speakerId: number) =>
    call<BlockInfo>('set_block_speaker', { libraryId, blockId, speakerId }),
  setActiveTranscript: (libraryId: number, callId: number, transcriptId: number) =>
    call<void>('set_active_transcript', { libraryId, callId, transcriptId }),
  history: (libraryId: number, callId: number, limit = 100) =>
    call<HistoryEntry[]>('history', { libraryId, callId, limit }),
  undo: (libraryId: number, callId: number) => call<HistoryEntry | null>('undo', { libraryId, callId }),
  search: (query: string, limit = 60) => call<SearchHit[]>('search_all', { query, limit }),
  assign: (libraryId: number, callId: number, toLibraryId: number, clientId: number | null) =>
    call<{ library_id: number; call_id: number }>('assign', { libraryId, callId, toLibraryId, clientId }),
  setSetting: (key: string, value: string | null) => call<void>('set_setting', { key, value }),
  reclaimable: () => call<Reclaimable>('reclaimable'),
  importPreview: (paths: string[], libraryId: number | null) =>
    call<ImportReport>('import_preview', { paths, libraryId }),
  importStart: (paths: string[], libraryId: number | null, clientId: number | null, convertAudio: boolean) =>
    call<void>('import_start', { paths, libraryId, clientId, convertAudio }),
  glossaryList: (libraryId: number | null, clientId: number | null, kind?: RuleKind) =>
    call<Rule[]>('glossary_list', { libraryId, clientId, kind: kind ?? null }),
  glossaryAdd: (a: RuleInput & { scope: Scope; libraryId?: number | null; clientId?: number | null; sourceEditId?: number | null }) =>
    call<Rule>('glossary_add', {
      scope: a.scope, libraryId: a.libraryId ?? null, clientId: a.clientId ?? null, kind: a.kind, pattern: a.pattern,
      replacement: a.replacement, caseSensitive: a.caseSensitive, sourceEditId: a.sourceEditId ?? null,
    }),
  glossaryUpdate: (a: RuleInput & { scope: Scope; libraryId?: number | null; id: number }) =>
    call<Rule>('glossary_update', {
      scope: a.scope, libraryId: a.libraryId ?? null, id: a.id, kind: a.kind, pattern: a.pattern,
      replacement: a.replacement, caseSensitive: a.caseSensitive,
    }),
  glossaryRemove: (scope: Scope, libraryId: number | null, id: number) =>
    call<Rule>('glossary_remove', { scope, libraryId, id }),
  glossaryPromote: (libraryId: number, id: number) => call<Rule>('glossary_promote', { libraryId, id }),
  glossaryApply: (libraryId: number, callId: number, transcriptId: number | null, dryRun: boolean) =>
    call<ApplyReport>('glossary_apply', { libraryId, callId, transcriptId, dryRun }),
  glossaryImportFile: (a: { path: string; scope: Scope; libraryId?: number | null; clientId?: number | null; kind?: RuleKind | null; dryRun: boolean }) =>
    call<GlossaryImportReport>('glossary_import_file', {
      path: a.path, scope: a.scope, libraryId: a.libraryId ?? null, clientId: a.clientId ?? null, kind: a.kind ?? null, dryRun: a.dryRun,
    }),
  glossarySuggestions: (libraryId: number, blockId: number, oldText: string, newText: string) =>
    call<BlockSuggestion[]>('glossary_suggestions', { libraryId, blockId, oldText, newText }),
  glossaryPromptTerms: (libraryId: number, clientId: number | null) =>
    call<PromptTerms>('glossary_prompt_terms', { libraryId, clientId }),
  // ---- gravação
  recordInfo: () => call<RecordInfo>('record_info'),
  recordDevices: () => call<RecordDevices>('record_devices'),
  recordStatus: () => call<RecordStatus>('record_status'),
  recordStart: (a: RecordStartArgs = {}) =>
    call<RecordStatus>('record_start', {
      libraryId: a.libraryId ?? null, clientId: a.clientId ?? null, title: a.title ?? null,
      expectedSpeakers: a.expectedSpeakers ?? null, language: a.language ?? null,
      mic: a.mic ?? null, sys: a.sys ?? null,
    }),
  /** substitui alvo, título, falantes e idioma da gravação em andamento (mande todos) */
  recordUpdate: (a: Omit<RecordStartArgs, 'mic' | 'sys'>) =>
    call<RecordStatus>('record_update', {
      libraryId: a.libraryId ?? null, clientId: a.clientId ?? null, title: a.title ?? null,
      expectedSpeakers: a.expectedSpeakers ?? null, language: a.language ?? null,
    }),
  recordStop: () => call<RecordStatus>('record_stop'),
  recordToggle: () => call<RecordStatus>('record_toggle'),
  recordMonitorStart: (mic: StreamChoice = 'default', sys: StreamChoice = 'default') =>
    call<void>('record_monitor_start', { mic, sys }),
  recordMonitorStop: () => call<void>('record_monitor_stop'),
  recordOrphans: () => call<Orphan[]>('record_orphans'),
  recordRecover: (key: string) => call<void>('record_recover', { key }),
  recordDiscard: (key: string) => call<void>('record_discard', { key }),
  recordSetShortcut: (accelerator: string | null) => call<ShortcutInfo>('record_set_shortcut', { accelerator }),
  barShow: () => call<void>('bar_show'),
  barHide: () => call<void>('bar_hide'),
  showMainWindow: () => call<void>('show_main_window'),
}

export async function on<T>(event: string, fn: (payload: T) => void): Promise<UnlistenFn> {
  await ready
  return listen<T>(event, e => fn(e.payload))
}

/** Seletor de pasta nativo (no navegador, um prompt). */
export async function pickFolder(title: string): Promise<string | null> {
  if (!inTauri) return window.prompt(title)
  const { open } = await import('@tauri-apps/plugin-dialog')
  const r = await open({ directory: true, multiple: false, title })
  return typeof r === 'string' ? r : null
}

/** Seletor de arquivo nativo (no navegador, um prompt: o mock ignora o conteúdo do caminho). */
export async function pickFile(title: string): Promise<string | null> {
  if (!inTauri) return window.prompt(title, '/exemplo/glossario.txt')
  const { open } = await import('@tauri-apps/plugin-dialog')
  const r = await open({ directory: false, multiple: false, title })
  return typeof r === 'string' ? r : null
}

/** Abre um link no navegador do sistema (no navegador comum o próprio <a target="_blank"> cuida disso). */
export async function openExternal(url: string): Promise<void> {
  if (!inTauri) { window.open(url, '_blank', 'noopener,noreferrer'); return }
  const { openUrl } = await import('@tauri-apps/plugin-opener')
  await openUrl(url)
}
