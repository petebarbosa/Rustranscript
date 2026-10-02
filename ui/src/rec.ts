// Estado de gravação compartilhado pela janela principal e pela mini barra (cada janela tem a sua instância).
// Só depende dos eventos `record-state` / `record-levels` / `record-finalize-*` (a barra só tem `core:event:default`)
// e dos comandos `record_*` próprios do app. Nada aqui desenha tela: as views assinam as mudanças.
import { t } from './i18n'
import { api, on, REC_EVENTS, type FinalizeDone, type FinalizeProgress, type LevelsEvent, type RecordStatus, type StreamLevel } from './api'

export interface Progress { stage: FinalizeProgress['stage']; track?: 'mic' | 'sys'; done: number; of: number }
export type Kind = 'state' | 'levels' | 'progress'

export const hub = {
  status: null as RecordStatus | null,
  /** últimos níveis (null = nada chegou há pouco) */
  levels: null as { source: LevelsEvent['source']; mic: StreamLevel | null; sys: StreamLevel | null } | null,
  /** progresso por chave de gravação (finalização/recuperação em andamento) */
  progress: new Map<string, Progress>(),
  /** última finalização concluída com sucesso (a tela de gravar mostra o link) */
  lastDone: null as FinalizeDone | null,
}

const subs: Record<Kind, Set<() => void>> = { state: new Set(), levels: new Set(), progress: new Set() }
const notify = (k: Kind) => subs[k].forEach(f => f())
export function subscribe(kind: Kind, fn: () => void): () => void {
  subs[kind].add(fn)
  return () => subs[kind].delete(fn)
}

// âncora do cronômetro: o shell manda `elapsed_s` nos eventos; entre um e outro o relógio local interpola
let anchor = { elapsed: 0, at: performance.now() }
export const isRecording = () => hub.status?.state === 'recording'
export const isBusy = () => isRecording() || (hub.status?.finalizing.length ?? 0) > 0
export function elapsedNow(): number {
  if (!isRecording()) return 0
  return anchor.elapsed + (performance.now() - anchor.at) / 1000
}

function setStatus(s: RecordStatus) {
  hub.status = s
  if (s.recording) anchor = { elapsed: s.recording.elapsed_s, at: performance.now() }
  else hub.levels = null
  notify('state')
}

let lastLevelsAt = 0
let started = false
const unlisten: (() => void)[] = []

/** Liga os eventos e busca o estado atual. Idempotente. */
export async function startHub(): Promise<void> {
  if (started) return
  started = true
  unlisten.push(
    await on<RecordStatus>(REC_EVENTS.state, setStatus),
    await on<LevelsEvent>(REC_EVENTS.levels, e => {
      hub.levels = { source: e.source, mic: e.mic, sys: e.sys }
      if (e.elapsed_s != null) anchor = { elapsed: e.elapsed_s, at: performance.now() }
      lastLevelsAt = performance.now()
      notify('levels')
    }),
    await on<FinalizeProgress>(REC_EVENTS.finalizeProgress, p => {
      hub.progress.set(p.key, {
        stage: p.stage, track: 'track' in p ? p.track : undefined,
        done: p.stage === 'convert' ? p.done : 0, of: p.stage === 'convert' ? p.of : 0,
      })
      notify('progress')
    }),
    await on<FinalizeDone>(REC_EVENTS.finalizeDone, d => {
      hub.progress.delete(d.key)
      if (d.ok) hub.lastDone = d
      notify('progress')
    }),
  )
  // sem eventos de nível por 1,5 s (monitor parado, estado mudou): zera os medidores
  const stale = setInterval(() => {
    if (hub.levels && performance.now() - lastLevelsAt > 1500) { hub.levels = null; notify('levels') }
  }, 500)
  unlisten.push(() => clearInterval(stale))
  try { setStatus(await api.recordStatus()) } catch { /* barra/sem backend: o primeiro record-state resolve */ }
}

/** Fração 0–1 do progresso de uma chave (reparo e chamada contam como etapas; a conversão, por trilha). */
export function progressFrac(p: Progress): number {
  if (p.stage === 'convert') return Math.min(1, p.done / Math.max(1, p.of))
  return p.stage === 'call' ? 1 : 0.05
}

/** Texto da etapa ("Convertendo para FLAC (Microfone) 40%"); sem progresso ainda = na fila. */
export function stageLabel(p: Progress | undefined): string {
  if (!p) return t('record.stage_wait')
  const track = p.track ? t(p.track === 'mic' ? 'record.track_mic' : 'record.track_sys') : ''
  if (p.stage === 'repair') return t('record.stage_repair', { track })
  if (p.stage === 'convert') return t('record.stage_convert', { track, pct: Math.round(progressFrac(p) * 100) })
  return t('record.stage_call')
}
