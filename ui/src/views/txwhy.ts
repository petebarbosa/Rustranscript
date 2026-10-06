// Por que uma tarefa não anda (`blocked_by` da fila) e o que deu errado quando falhou: texto + ação, nas telas da fila e da chamada.
import { api, type JobInfo } from '../api'
import { t } from '../i18n'
import { btnCls, describeError } from '../dialogs'
import { jobError, startSetup } from '../tx'
import { esc, toast } from '../util'

type Why = { text: string; action: 'setup' | 'resume' | null; label: string }

/** Texto (e ação, se houver) do porquê de uma tarefa `queued` ainda não ter começado. */
export function queuedWhy(j: JobInfo): Why {
  const by = j.blocked_by
  const text = (k: string) => t(`transcription.why.${k}`)
  switch (by) {
    case 'behind': return { text: t('transcription.why.behind', { n: j.ahead ?? 1 }), action: null, label: '' }
    case 'paused_user': return { text: text('paused_user'), action: 'resume', label: t('queue.resume') }
    case 'paused_recording': return { text: text('paused_recording'), action: null, label: '' }
    case 'runtime_installing': return { text: text('runtime_installing'), action: null, label: '' }
    case 'runtime_missing': return { text: text('runtime_missing'), action: 'setup', label: t('transcription.engine.install') }
    case 'runtime_outdated': return { text: text('runtime_outdated'), action: 'setup', label: t('transcription.engine.update') }
    case 'models_missing': return { text: text('models_missing'), action: 'setup', label: t('transcription.engine.download_models') }
    default: return { text: t('transcription.queued_body'), action: null, label: '' }
  }
}

/** Parágrafo com o motivo e, quando dá para resolver ali mesmo, o botão (`data-why`; `onWhyClick` trata o clique). */
export function whyHtml(j: JobInfo, opts: { center?: boolean; textCls?: string; noAction?: boolean } = {}): string {
  const w = queuedWhy(j)
  const warn = j.blocked_by && j.blocked_by !== 'behind'
  const textCls = opts.textCls ?? (warn ? 'text-amber-200' : 'text-zinc-400')
  return `<div data-why-box class="${opts.center ? 'mx-auto' : ''} max-w-xl ${opts.center ? 'text-center' : ''}">
    <p class="text-sm ${textCls}">${esc(w.text)}</p>
    ${w.action && !opts.noAction ? `<button type="button" data-why="${w.action}" class="mt-3 ${w.action === 'setup' ? btnCls.btnPrimary : btnCls.btn}">${esc(w.label)}</button>` : ''}</div>`
}

/** Trata o clique num `data-why`; `true` se era um. */
export function onWhyClick(target: HTMLElement): boolean {
  const act = target.closest<HTMLElement>('[data-why]')?.dataset.why
  if (!act) return false
  if (act === 'setup') void startSetup()
  else void api.queuePause(false).catch(e => toast(describeError(e), 'err'))
  return true
}

/** Erro de uma tarefa que falhou: frase legível do `error_code` e, num "detalhes", o código e o `error_detail`. */
export function errorHtml(j: Pick<JobInfo, 'error_code' | 'error_detail'>, opts: { center?: boolean } = {}): string {
  const e = jobError(j)
  const c = opts.center ? 'mx-auto mt-2' : ''
  return `<p class="${c} max-w-xl text-sm text-rose-200">${esc(e.title)}</p>
    <details class="${c} ${opts.center ? 'text-center' : 'mt-2 text-left'} max-w-xl"><summary class="cursor-pointer text-xs text-zinc-500 hover:text-zinc-300">${esc(t('transcription.error_details'))}</summary>
      <p class="mt-1.5 font-mono text-xs text-zinc-500">${esc(t('transcription.error_code', { code: e.code }))}</p>
      ${e.detail ? `<pre class="mt-1 whitespace-pre-wrap break-words font-mono text-xs text-zinc-500">${esc(e.detail)}</pre>` : ''}</details>`
}
