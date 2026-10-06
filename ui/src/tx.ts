// Estado da transcrição compartilhado pelas telas (fila, configurações, chamada, barra lateral).
// Só depende dos eventos `queue-changed` / `queue-progress` / `transcription-setup` e dos comandos do §5 do contrato.
// Nada aqui desenha tela: as views assinam as mudanças (mesmo padrão de rec.ts).
import { t } from './i18n'
import { api, on, toError, TRANSCRIPTION_EVENTS, type ApiError, type JobInfo, type JobKind, type JobOptions, type JobProgress, type QueueStatus, type SetupEvent, type TranscriptionStatus } from './api'
import { confirmDialog, describeError } from './dialogs'
import { fmtTime, toast } from './util'

export type Kind = 'queue' | 'progress' | 'setup' | 'status'

export interface SetupState {
  running: boolean
  phase: SetupEvent['phase'] | null
  step: SetupEvent['step']
  index: number | null; of: number | null
  model: string | null; file: string | null; bytes_done: number | null; bytes_total: number | null
  /** erro da última execução (setup_cancelled = cancelada pelo usuário) */
  error: ApiError | null
  /** terminou com sucesso nesta sessão */
  ok: boolean
}

const idle = (): SetupState => ({ running: false, phase: null, step: null, index: null, of: null, model: null, file: null, bytes_done: null, bytes_total: null, error: null, ok: false })

export const tx = {
  status: null as TranscriptionStatus | null,
  queue: { paused: null, blocked_by: null, jobs: [] } as QueueStatus,
  progress: new Map<number, JobProgress>(),
  setup: idle(),
}

const subs: Record<Kind, Set<() => void>> = { queue: new Set(), progress: new Set(), setup: new Set(), status: new Set() }
const notify = (k: Kind) => subs[k].forEach(f => f())
export function subscribe(kind: Kind, fn: () => void): () => void {
  subs[kind].add(fn)
  return () => subs[kind].delete(fn)
}

let started = false

function setQueue(q: QueueStatus) {
  // o motivo da fila mudou (runtime atualizado por fora, instalação começou/acabou...): o estado do motor também pode ter mudado
  const reasonChanged = q.blocked_by !== tx.queue.blocked_by
  tx.queue = q
  // progresso só vale para a tarefa rodando e na mesma etapa (a etapa nova chega no próximo `queue-progress`)
  for (const [id, p] of tx.progress) {
    const j = q.jobs.find(x => x.id === id)
    if (!j || j.state !== 'running' || j.stage !== p.stage) tx.progress.delete(id)
  }
  if (tx.status) tx.status.queue = q
  notify('queue')
  if (reasonChanged) void refreshStatus()
}

function setSetupEvent(e: SetupEvent) {
  const s = tx.setup
  if (e.phase === 'finished') {
    Object.assign(tx.setup, idle(), { error: e.error, ok: !e.error })
    void refreshStatus()
  } else {
    const toModels = e.phase === 'models' && s.phase === 'runtime'
    Object.assign(s, { running: true, phase: e.phase, step: e.step, index: e.index, of: e.of, model: e.model, file: e.file, bytes_done: e.bytes_done, bytes_total: e.bytes_total, error: null, ok: false })
    if (toModels) void refreshStatus() // o ambiente já está pronto: a linha dele muda enquanto os modelos baixam
  }
  notify('setup')
}

export async function refreshStatus(): Promise<TranscriptionStatus | null> {
  try {
    const st = await api.transcriptionStatus()
    // a releitura periódica (`watchStatus`) não redesenha as telas se nada mudou (preserva "detalhes" abertos e seleção)
    const same = !!tx.status && JSON.stringify(tx.status) === JSON.stringify(st)
    tx.status = st
    tx.queue = st.queue
    const was = tx.setup.running
    if (st.setup.running && !tx.setup.running) { Object.assign(tx.setup, idle(), { running: true, phase: st.setup.phase }) }
    if (!st.setup.running && tx.setup.running && tx.setup.phase !== null) tx.setup.running = false
    if (same && was === tx.setup.running) return tx.status
    notify('status'); notify('queue'); notify('setup')
  } catch { /* sem backend de transcrição: a UI segue sem fila */ }
  return tx.status
}

/**
 * Mantém `tx.status` (motor e modelos) fresco enquanto uma tela o mostra: relê agora e a cada `every` ms.
 * O estado do motor muda sem evento (a CLI instala por fora; o `worker.py` é atualizado sozinho). Devolve quem desliga.
 */
export function watchStatus(every = 5000): () => void {
  void refreshStatus()
  const id = setInterval(() => { if (!document.hidden) void refreshStatus() }, every)
  return () => clearInterval(id)
}

/** Instala/atualiza o motor e baixa os modelos que faltam (o mesmo fluxo do botão "Baixar e instalar"). */
export async function startSetup(): Promise<void> {
  Object.assign(tx.setup, { running: true, phase: 'runtime', step: null, index: null, of: null, error: null, ok: false })
  notify('setup')
  try { await api.transcriptionSetupStart() }
  catch (e) {
    if (toError(e).code !== 'conflict') { tx.setup.running = false; tx.setup.error = toError(e); notify('setup') }
  }
}

/** Liga os eventos e busca o estado atual. Idempotente. */
export async function startTx(): Promise<void> {
  if (started) return
  started = true
  await on<QueueStatus>(TRANSCRIPTION_EVENTS.queueChanged, setQueue)
  await on<JobProgress>(TRANSCRIPTION_EVENTS.queueProgress, p => { tx.progress.set(p.job_id, p); notify('progress') })
  await on<SetupEvent>(TRANSCRIPTION_EVENTS.setup, setSetupEvent)
  await refreshStatus()
}

// ---------------------------------------------------------------- consultas

/** Pronta para rodar: modo de teste, ou ambiente e os 3 modelos instalados. */
export const isReady = (st: TranscriptionStatus | null = tx.status) =>
  !!st && (st.fake_worker || (st.runtime.state === 'ready' && st.models.every(m => m.installed)))

export const openJobs = () => tx.queue.jobs.filter(j => j.state === 'queued' || j.state === 'running')

/** Tarefa aberta da chamada (uma por chamada); senão a última que falhou. */
export function jobForCall(libraryId: number, callId: number): JobInfo | undefined {
  const mine = tx.queue.jobs.filter(j => j.library_id === libraryId && j.call_id === callId)
  return mine.find(j => j.state === 'running') ?? mine.find(j => j.state === 'queued') ?? mine.filter(j => j.state === 'failed').sort((a, b) => b.id - a.id)[0]
}

/**
 * Etapa e fração atuais da tarefa: o último `queue-progress` vence o que veio no `queue-changed`.
 * Diarização: o worker real só reporta a segmentação; ao chegar em 100 % o resto (embeddings, agrupamento) não
 * tem progresso até o resultado, então a barra volta a ser indeterminada em vez de ficar parada em 100 %.
 */
export function jobProgress(j: JobInfo): { stage: JobInfo['stage']; fraction: number | null; audio_s: number | null; total_s: number | null } {
  const p = tx.progress.get(j.id)
  const r = p && j.state === 'running'
    ? { stage: p.stage as JobInfo['stage'], fraction: p.fraction, audio_s: p.audio_s, total_s: p.total_s }
    : { stage: j.stage, fraction: j.progress, audio_s: null, total_s: null }
  if (r.stage === 'diarize' && r.fraction != null && r.fraction >= 1) r.fraction = null
  return r
}

/** "Transcrevendo o áudio do sistema · 12:30 de 42:00" / "Separando as vozes" (sem fração = só a etapa). */
export function stageText(j: JobInfo): string {
  const p = jobProgress(j)
  const name = t(`transcription.stage.${p.stage ?? 'preparing'}`)
  if (p.audio_s != null && p.total_s) return `${name} · ${t('queue.audio_of', { done: fmtTime(p.audio_s), total: fmtTime(p.total_s) })}`
  if (p.fraction != null) return `${name} · ${Math.round(p.fraction * 100)}%`
  return name
}

export function jobError(j: Pick<JobInfo, 'error_code' | 'error_detail'>): { title: string; code: string; detail: string } {
  const code = j.error_code ?? 'job_failed'
  const k = `error.${code}`
  const title = t(k) === k ? t('error.job_failed') : t(k)
  return { title, code, detail: j.error_detail ?? '' }
}

/** Motor que precisa de ação do usuário (instalar/atualizar); `fake` e `ready` não. */
export const engineNeedsSetup = (st: TranscriptionStatus | null = tx.status) => !!st && !st.fake_worker && (st.runtime.state === 'missing' || st.runtime.state === 'outdated')

// ---------------------------------------------------------------- ações (confirmação + aviso de erro)

/** Cancela com confirmação (o progresso parcial é descartado). */
export async function cancelJob(j: JobInfo): Promise<boolean> {
  if (!(await confirmDialog(t('queue.cancel'), t('queue.cancel_confirm'), t('queue.cancel_ok'), t('common.back')))) return false
  try { await api.queueCancel(j.id); return true } catch (e) { toast(describeError(e), 'err'); return false }
}

export async function retryJob(j: JobInfo): Promise<boolean> {
  try { await api.queueRetry(j.id); toast(t('queue.enqueued')); return true } catch (e) { toast(describeError(e), 'err'); return false }
}

/** Enfileira uma chamada; 'conflict' = já tem tarefa aberta. */
export async function enqueueCall(libraryId: number, callId: number, kind: JobKind = 'full', options: JobOptions | null = null): Promise<boolean> {
  try { await api.transcribeEnqueue(libraryId, callId, kind, options); toast(t('queue.enqueued')); return true }
  catch (e) { toast(toError(e).code === 'conflict' ? t('queue.already_open') : describeError(e), 'err'); return false }
}
