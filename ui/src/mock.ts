// Backend falso para abrir a UI no navegador (`npm run dev`) e testar com agent-browser.
// Dados 100% sintéticos. Nunca é carregado dentro do Tauri.
import { mockIPC } from '@tauri-apps/api/mocks'
import { emit } from '@tauri-apps/api/event'
import type { ApplyReport, BlockChange, BlockInfo, BlockSuggestion, CallDetail, ClientInfo, DeviceInfo, GlossaryImportEntry, HistoryEntry, Hit, LibraryInfo, LevelsEvent, Orphan, RecordingInfo, RecordStatus, Rule, SpeakerInfo, StreamChoice, StreamLevel, StreamStatus } from './api'

type Args = Record<string, any>

interface Call extends Omit<CallDetail, 'library_name' | 'words' | 'preview' | 'edited_blocks' | 'transcript_id'> {
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
    id: ++seq, seq: i + 1, t_start: t, t_end: t, speaker_id: sid(spk), text, original_text: text, edited: false,
  }))
  const date = key.slice(5, 15), time = key.slice(16).replace(/-/g, ':')
  return {
    library_id, id, key, title, client_id, client_name: null, started_at: `${date}T${time}`,
    duration_s: lines[lines.length - 1][0] + 30, versions: 1, has_audio: true, language: 'pt', expected_speakers: null,
    transcription_state: 'done', transcription_error: null,
    transcript_id: 1, transcripts: [{ id: 1, version: 1, model: 'large-v3-turbo', engine: 'faster-whisper', source_file: key + '.txt', created_at: now(), is_active: true }],
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
  c.transcripts.push({ id, version: 2, model, engine: 'faster-whisper', source_file: c.key + '_v2.txt', created_at: now(), is_active: false })
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
function summary(c: Call) {
  const words = c.blocks.reduce((n, b) => n + b.text.split(/\s+/).filter(Boolean).length, 0)
  const preview = c.blocks.slice(0, 3).map(b => b.text).join(' ').slice(0, 200)
  const client = clients.find(x => x.id === c.client_id)
  const { speakers: _s, blocks: _b, chapters: _c, transcripts: _t, audio: _a, other: _o, ...rest } = c
  return { ...rest, client_name: client?.name ?? null, words, preview, edited_blocks: c.blocks.filter(b => b.edited).length }
}
function record(library_id: number, call_id: number, entity: HistoryEntry['entity'], entity_id: number, old_value: string | null, new_value: string | null, batch?: { id: number; size: number }) {
  const id = ++seq
  history.push({
    library_id, id, call_id, entity, entity_id, old_value, new_value, origin: 'ui', at: now(), undone_at: null,
    batch_id: batch?.id ?? null, batch_kind: batch ? 'glossary' : null, batch_size: batch?.size ?? null,
  })
  return id
}
const norm = (s: string) => s.replace(/\s+/g, ' ').trim()
const fold = (s: string) => s.normalize('NFD').replace(/\p{M}/gu, '').toLowerCase()

const handlers: Record<string, (a: Args) => unknown> = {
  bootstrap: () => ({ data_dir: '/dados', system_language: 'pt-BR', inbox_id: 1, settings, libraries: handlers.libraries({}) }),
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
    const { other: _o, ...full } = c
    if (a.transcriptId != null && c.transcript_id == null) throw { code: 'not_found', detail: `transcript ${a.transcriptId}` }
    return { ...full, ...summary(c), library_name: lib(c.library_id).name, blocks: c.blocks.map(b => ({ ...b })), speakers: c.speakers.map(s => ({ ...s })) }
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
      for (const b of c.blocks) if (hit(b.text)) out.push({ library_id: c.library_id, call_id: c.id, call_key: c.key, call_title: c.title, started_at: c.started_at, block_id: b.id, t_start: b.t_start, snippet: mark(b.text), rank: -1 })
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
    if (!['language', 'me_name', 'transcription_language', 'last_library_id', 'last_client_id', 'record_library_id', 'record_client_id', 'record_mic', 'record_sys', 'record_bar_on_start'].includes(a.key)) throw { code: 'invalid', detail: `unknown setting ${a.key}` }
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
    occurrences_in_call: c.blocks.filter(b => b.id !== blockId && re.test(b.text)).length,
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
    for (const b of c.blocks) {
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
})

if (flags.has('orphans')) seedOrphans()

export function install() {
  mockIPC(async (cmd, args) => {
    const h = handlers[cmd]
    if (!h) throw { code: 'unknown', detail: `mock: ${cmd}` }
    return structuredClone(await h((args ?? {}) as Args))
  }, { shouldMockEvents: true })
  ;(window as any).__mock = {
    calls, history, rules, rec,
    addOrphans: () => { seedOrphans(); emitOrphans() },
  }
  // startup real: o shell emite `record-recovery` se houver órfãs (a UI também pergunta por `record_orphans` no boot)
  if (rec.orphans.length) setTimeout(emitOrphans, 700)
}
