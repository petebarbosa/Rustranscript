// Apagar o áudio das chamadas (#24): confirmação e erros compartilhados pela tela da chamada e pela lista de armazenamento.
import { toError } from './api'
import { describeError, form } from './dialogs'
import { t } from './i18n'
import { esc, fmtBytes } from './util'

/** Texto do erro: "gravando ou tarefa aberta" (`conflict`) tem explicação própria; o resto segue o padrão. */
export const audioError = (e: unknown) => (toError(e).code === 'conflict' ? t('audio.err_busy') : describeError(e))

/**
 * Confirmação da exclusão (irreversível): mostra o tamanho que será liberado e o que deixa de funcionar.
 * `run` faz a exclusão; erro dele fica no próprio diálogo (que continua aberto). Cancelar devolve `null`.
 */
export function confirmAudioDelete<T>(title: string, bytes: number, run: () => Promise<T>): Promise<T | null> {
  const li = (key: string) => `<li>${esc(t(key))}</li>`
  const body = `<p class="text-sm font-medium text-zinc-100">${esc(t('audio.frees', { size: fmtBytes(bytes) }))}</p>
    <p class="text-sm text-zinc-400">${esc(t('audio.warn_intro'))}</p>
    <ul class="list-disc space-y-1 pl-5 text-sm text-zinc-300">${li('audio.warn_listen')}${li('audio.warn_cut')}${li('audio.warn_redo')}</ul>
    <p class="text-sm font-medium text-rose-300">${esc(t('audio.irreversible'))}</p>`
  return form(title, body, t('audio.delete'), async () => {
    try { return await run() }
    catch (e) { throw { code: 'audio_delete_failed', detail: audioError(e) } }
  }, f => {
    f.querySelector('[type="submit"]')!.className = 'rounded-xl border border-rose-400/60 bg-rose-500/20 px-4 py-2 text-sm font-medium text-rose-50 hover:bg-rose-500/30'
  })
}
