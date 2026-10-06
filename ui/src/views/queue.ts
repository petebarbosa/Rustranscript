// Fila de transcrição: estado de cada tarefa (etapa + barra), pausar/retomar, cancelar, tentar de novo.
import { api, type CallSummary, type JobInfo } from '../api'
import { t } from '../i18n'
import { type View } from '../store'
import { describeError } from '../dialogs'
import { cancelJob, isReady, jobProgress, retryJob, stageText, subscribe, tx, watchStatus } from '../tx'
import { barHtml, callTitle, esc, fmtClock, fmtDate, toast } from '../util'
import { mountSetup } from './txsetup'
import { errorHtml, whyHtml } from './txwhy'

const stateTone: Record<JobInfo['state'], string> = {
  queued: 'bg-amber-400/10 text-amber-300', running: 'bg-sky-400/10 text-sky-300', done: 'bg-emerald-400/10 text-emerald-300',
  failed: 'bg-rose-400/10 text-rose-300', cancelled: 'bg-white/5 text-zinc-400',
}
const btn = 'rounded-lg border border-white/10 bg-ink-800 px-3 py-1.5 text-xs text-zinc-200 hover:border-violet-400/50'

export async function renderQueue(el: HTMLElement): Promise<View> {
  let titles = new Map<string, CallSummary>()
  const loadTitles = async () => { titles = new Map((await api.calls(null, null, false)).map(c => [`${c.library_id}:${c.id}`, c])) }
  await loadTitles()
  let offSetup: (() => void) | null = null

  const titleOf = (j: JobInfo) => {
    const c = titles.get(`${j.library_id}:${j.call_id}`)
    return c ? callTitle(c) : j.call_key
  }

  /** Corpo variável da tarefa: progresso (rodando), espera (na fila) ou erro (falhou). */
  function bodyHtml(j: JobInfo): string {
    if (j.state === 'running') {
      const p = jobProgress(j)
      return `<p class="text-sm text-zinc-300">${esc(stageText(j))}</p><div class="mt-2">${barHtml(p.fraction, 'bg-sky-400')}</div>`
    }
    // o botão de instalar/atualizar fica no cartão do motor, na mesma tela: aqui só o motivo
    if (j.state === 'queued') return whyHtml(j, { noAction: true })
    if (j.state === 'failed') return errorHtml(j)
    return ''
  }

  function rowHtml(j: JobInfo): string {
    const open = j.state === 'queued' || j.state === 'running'
    const date = titles.get(`${j.library_id}:${j.call_id}`)?.started_at
    return `<li data-job="${j.id}" class="rounded-2xl border ${j.state === 'failed' ? 'border-rose-400/25' : 'border-white/10'} bg-ink-900/70 p-4">
      <div class="flex flex-wrap items-start gap-3">
        <div class="min-w-0 flex-1">
          <a href="#/call/${j.library_id}/${j.call_id}" class="block truncate font-medium text-white hover:text-violet-200">${esc(titleOf(j))}</a>
          <div class="mt-1.5 flex flex-wrap items-center gap-2 text-xs text-zinc-500">
            <span class="rounded-full px-2.5 py-0.5 ${stateTone[j.state]}">${esc(t(`queue.state.${j.state}`))}</span>
            <span>${esc(t(`queue.kind.${j.kind}`))}</span>
            ${j.attempts > 1 ? `<span class="whitespace-nowrap">· ${esc(t('queue.attempt', { n: j.attempts }))}</span>` : ''}
            ${date ? `<span class="whitespace-nowrap">· ${esc(fmtDate(date))} ${esc(fmtClock(date))}</span>` : ''}
          </div>
        </div>
        <div class="flex shrink-0 gap-2">
          ${open ? `<button type="button" data-cancel="${j.id}" class="${btn}">${esc(t('queue.cancel'))}</button>` : ''}
          ${j.state === 'failed' || j.state === 'cancelled' ? `<button type="button" data-retry="${j.id}" class="${btn}">${esc(t('queue.retry'))}</button>` : ''}
        </div>
      </div>
      <div class="mt-3 empty:hidden" data-body>${bodyHtml(j)}</div></li>`
  }

  function banner(text: string, tone: 'amber' | 'zinc', id: string, extra = '') {
    const cls = tone === 'amber' ? 'border-amber-400/25 bg-amber-400/[0.05] text-amber-200' : 'border-white/10 bg-white/[0.03] text-zinc-400'
    return `<p id="${id}" class="mt-4 rounded-xl border px-4 py-2.5 text-sm ${cls}">${esc(text)}${extra}</p>`
  }

  function draw() {
    offSetup?.(); offSetup = null
    const jobs = tx.queue.jobs
    const active = jobs.filter(j => j.state === 'running' || j.state === 'queued')
    const recent = jobs.filter(j => j.state !== 'running' && j.state !== 'queued')
    const paused = tx.queue.paused
    const ready = isReady()
    el.innerHTML = `<div class="mx-auto max-w-3xl px-6 py-10">
      <div class="flex flex-wrap items-center justify-between gap-3">
        <h1 class="text-3xl font-semibold tracking-tight text-white">${esc(t('queue.title'))}</h1>
        <button type="button" id="pause" class="${btn} !px-4 !py-2 !text-sm">${esc(paused === 'user' ? t('queue.resume') : t('queue.pause'))}</button>
      </div>
      ${paused === 'user' ? banner(t('queue.paused_user'), 'amber', 'banner-paused') : ''}
      ${paused === 'recording' ? banner(t('queue.paused_recording'), 'amber', 'banner-recording') : ''}
      ${tx.status?.fake_worker ? banner(t('transcription.fake_worker'), 'zinc', 'banner-fake') : ''}
      ${ready ? '' : `<div id="queue-setup" class="mt-4"></div>`}
      <ul id="active" class="mt-6 space-y-3">${active.map(rowHtml).join('') || `<li class="rounded-2xl border border-dashed border-white/10 p-8 text-center text-sm text-zinc-500">${esc(t('queue.empty'))}</li>`}</ul>
      ${recent.length ? `<h2 class="mb-3 mt-10 text-xs font-semibold uppercase tracking-wider text-zinc-500">${esc(t('queue.recent'))}</h2><ul id="recent" class="space-y-3">${recent.map(rowHtml).join('')}</ul>` : ''}
    </div>`
    const box = el.querySelector<HTMLElement>('#queue-setup')
    if (box) offSetup = mountSetup(box)
    el.querySelector('#pause')!.addEventListener('click', async () => {
      try { await api.queuePause(paused !== 'user') } catch (e) { toast(describeError(e), 'err') }
    })
    el.querySelectorAll<HTMLElement>('[data-cancel]').forEach(b => b.addEventListener('click', () => {
      const j = tx.queue.jobs.find(x => x.id === Number(b.dataset.cancel))
      if (j) void cancelJob(j)
    }))
    el.querySelectorAll<HTMLElement>('[data-retry]').forEach(b => b.addEventListener('click', () => {
      const j = tx.queue.jobs.find(x => x.id === Number(b.dataset.retry))
      if (j) void retryJob(j)
    }))
  }

  /** Só o texto/barra da tarefa rodando (≤ 4×/s): não recria botões. */
  function paintProgress() {
    for (const j of tx.queue.jobs) {
      if (j.state !== 'running') continue
      const body = el.querySelector<HTMLElement>(`[data-job="${j.id}"] [data-body]`)
      if (body) body.innerHTML = bodyHtml(j)
    }
  }

  const offs = [
    subscribe('queue', async () => {
      if (tx.queue.jobs.some(j => !titles.has(`${j.library_id}:${j.call_id}`))) await loadTitles().catch(() => {})
      draw()
    }),
    subscribe('progress', paintProgress),
    subscribe('status', () => { if (!!el.querySelector('#queue-setup') === isReady()) draw() }),
    // o estado do motor muda sem evento (instalação pela CLI, atualização do script): reler ao abrir e de tempos em tempos
    watchStatus(),
  ]
  draw()
  return {
    refresh: async () => { await loadTitles(); draw() },
    dispose: () => { offs.forEach(f => f()); offSetup?.() },
  }
}
