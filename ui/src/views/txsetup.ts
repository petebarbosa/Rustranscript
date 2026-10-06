// Cartão "Preparar a transcrição": estado do ambiente e dos modelos, instalação com progresso (eventos
// `transcription-setup`), cancelar/retomar e "usar arquivo local". Usado nas Configurações e na tela da fila.
import { api, pickFile, pickFolder, toError, type ModelStatus } from '../api'
import { t } from '../i18n'
import { btnCls, describeError } from '../dialogs'
import { isReady, refreshStatus, subscribe, tx } from '../tx'
import { barHtml, esc, fmtBytes, toast } from '../util'

const MODELS: ModelStatus['id'][] = ['whisper', 'segmentation', 'embedding']

function chip(text: string, tone: 'ok' | 'warn' | 'off') {
  const cls = tone === 'ok' ? 'bg-emerald-400/10 text-emerald-300' : tone === 'warn' ? 'bg-amber-400/10 text-amber-300' : 'bg-white/5 text-zinc-400'
  return `<span class="shrink-0 rounded-full px-2.5 py-0.5 text-xs ${cls}">${esc(text)}</span>`
}

function progressHtml(): string {
  const s = tx.setup
  if (s.phase === 'models') {
    const frac = s.bytes_total ? (s.bytes_done ?? 0) / s.bytes_total : null
    return `<div class="flex items-baseline justify-between gap-3 text-sm"><span class="min-w-0 truncate text-zinc-200">${esc(t('transcription.setup_phase_models'))}${s.index && s.of ? ` (${s.index}/${s.of})` : ''} · ${esc(s.model ? t(`transcription.model.${s.model}`) : '')}</span>
      <span class="shrink-0 font-mono text-xs tabular-nums text-zinc-400">${s.bytes_total ? `${fmtBytes(s.bytes_done ?? 0)} / ${fmtBytes(s.bytes_total)}` : ''}</span></div>
      <div class="mt-2">${barHtml(frac)}</div>
      ${s.file ? `<p class="mt-1.5 truncate font-mono text-xs text-zinc-500">${esc(s.file)}</p>` : ''}`
  }
  const frac = s.index && s.of ? (s.index - 1) / s.of : null
  return `<div class="flex items-baseline justify-between gap-3 text-sm"><span class="text-zinc-200">${esc(t('transcription.setup_phase_runtime'))}${s.step ? ` · ${esc(t(`transcription.step.${s.step}`))}` : ''}</span>
    <span class="shrink-0 font-mono text-xs tabular-nums text-zinc-400">${s.index && s.of ? `${s.index}/${s.of}` : ''}</span></div>
    <div class="mt-2">${barHtml(frac)}</div>`
}

/** Monta o cartão em `el` e o mantém atualizado. Devolve a função que desliga as assinaturas. */
export function mountSetup(el: HTMLElement): () => void {
  const draw = () => {
    const st = tx.status
    if (!st) { el.innerHTML = ''; return }
    const s = tx.setup
    const ready = isReady(st)
    const rt = st.runtime.state
    const partial = st.models.some(m => !m.installed && m.bytes_done > 0)
    const needed = st.models.filter(m => !m.installed).reduce((n, m) => n + Math.max(0, m.bytes_total - m.bytes_done), 0)
    const rows = MODELS.map(id => {
      const m = st.models.find(x => x.id === id)
      const state = !m ? chip(t('transcription.model_missing'), 'off')
        : m.installed ? chip(m.local ? `${t('transcription.model_installed')} · ${t('transcription.model_local')}` : t('transcription.model_installed'), 'ok')
        : m.bytes_done > 0 ? chip(`${fmtBytes(m.bytes_done)} / ${fmtBytes(m.bytes_total)}`, 'warn') : chip(t('transcription.model_missing'), 'off')
      return `<li class="flex flex-wrap items-center gap-x-3 gap-y-1.5 rounded-xl border border-white/10 bg-ink-950/50 px-4 py-2.5" data-model="${id}">
        <span class="min-w-0 basis-48 flex-1 text-sm text-zinc-200">${esc(t(`transcription.model.${id}`))}${m ? `<span class="ml-2 whitespace-nowrap text-xs text-zinc-500">${esc(fmtBytes(m.bytes_total))}</span>` : ''}</span>
        ${state}
        <button type="button" data-local="${id}" ${s.running ? 'disabled' : ''} class="rounded-lg px-2 py-1 text-xs text-zinc-400 hover:bg-white/5 hover:text-zinc-100 disabled:opacity-40">${esc(t('transcription.import_local'))}</button></li>`
    }).join('')
    const runtimeTone = rt === 'ready' ? 'ok' : rt === 'fake' ? 'warn' : rt === 'outdated' ? 'warn' : 'off'
    const msg = s.running ? '' : s.error
      ? (s.error.code === 'setup_cancelled'
        ? `<p data-msg class="mt-3 rounded-xl border border-amber-400/20 bg-amber-400/[0.04] px-3 py-2 text-sm text-amber-200">${esc(t('transcription.setup_cancelled'))}</p>`
        : `<p data-msg class="mt-3 rounded-xl border border-rose-400/30 bg-rose-400/[0.05] px-3 py-2 text-sm text-rose-200">${esc(t('transcription.setup_failed', { error: describeError(s.error) }))}</p>`)
      : s.ok ? `<p data-msg class="mt-3 rounded-xl border border-emerald-400/20 bg-emerald-400/[0.04] px-3 py-2 text-sm text-emerald-200">${esc(t('transcription.setup_done'))}</p>` : ''
    el.innerHTML = `<div id="tx-setup" class="rounded-2xl border ${ready ? 'border-white/10' : 'border-amber-400/25'} bg-ink-900/60 p-5">
      <h2 class="text-sm font-semibold text-white">${esc(ready ? t('settings.transcription.runtime') : t('transcription.setup_title'))}</h2>
      ${ready ? '' : `<p class="mt-1 text-sm text-zinc-400">${esc(t('transcription.setup_body'))}</p>`}
      <div class="mt-4 flex flex-wrap items-center gap-3 rounded-xl border border-white/10 bg-ink-950/50 px-4 py-2.5">
        <span class="min-w-0 flex-1 text-sm text-zinc-200">${esc(t('settings.transcription.runtime'))}${rt === 'ready' ? `<span class="ml-2 font-mono text-xs text-zinc-500">Python ${esc(st.runtime.python)}</span>` : ''}</span>
        ${chip(t(`transcription.runtime.${rt}`), runtimeTone)}</div>
      <ul class="mt-2 space-y-2">${rows}</ul>
      ${st.fake_worker ? `<p class="mt-3 text-xs text-amber-300">${esc(t('transcription.fake_worker'))}</p>` : ''}
      ${s.running ? `<div class="mt-4" data-progress>${progressHtml()}</div>` : ''}
      ${msg}
      ${ready && !s.running ? '' : `<div class="mt-4 flex flex-wrap items-center gap-3">
        ${s.running
          ? `<button type="button" id="setup-cancel" class="${btnCls.btn}">${esc(t('transcription.setup_cancel'))}</button><span class="text-xs text-zinc-500">${esc(t('transcription.setup_running'))}</span>`
          : `<button type="button" id="setup-start" class="${btnCls.btnPrimary}">${esc(partial || s.error?.code === 'setup_cancelled' ? t('transcription.setup_resume') : t('transcription.setup_start'))}</button>
             ${needed ? `<span class="text-xs text-zinc-500">${esc(t('transcription.setup_size', { size: fmtBytes(needed) }))}</span>` : ''}`}
      </div>`}
    </div>`
    el.querySelector('#setup-start')?.addEventListener('click', async () => {
      Object.assign(tx.setup, { running: true, phase: 'runtime', step: null, index: null, of: null, error: null, ok: false })
      draw()
      try { await api.transcriptionSetupStart() }
      catch (e) {
        if (toError(e).code !== 'conflict') { tx.setup.running = false; tx.setup.error = toError(e); draw() }
      }
    })
    el.querySelector('#setup-cancel')?.addEventListener('click', () => { api.transcriptionSetupCancel().catch(e => toast(describeError(e), 'err')) })
    el.querySelectorAll<HTMLElement>('[data-local]').forEach(b => b.addEventListener('click', async () => {
      const id = b.dataset.local as ModelStatus['id']
      // Whisper é uma pasta (modelo CTranslate2); segmentação e embedding são arquivos
      const path = id === 'whisper' ? await pickFolder(t('transcription.pick_dir')) : await pickFile(t('transcription.pick_file'))
      if (!path) return
      try {
        const models = await api.modelsImportLocal(id, path)
        if (tx.status) tx.status.models = models
        toast(t('transcription.import_ok'))
        await refreshStatus()
        draw()
      } catch (e) { toast(describeError(e), 'err') }
    }))
  }
  // progresso: troca só o bloco de progresso (não recria botões, que perderiam o foco)
  const onSetup = () => {
    const box = el.querySelector<HTMLElement>('[data-progress]')
    if (box && tx.setup.running) box.innerHTML = progressHtml()
    else draw()
  }
  const offs = [subscribe('setup', onSetup), subscribe('status', draw)]
  draw()
  return () => offs.forEach(f => f())
}
