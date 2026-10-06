// Configurações → Transcrição: instalação/modelos (cartão compartilhado) + opções. Chaves e padrões: contrato §3.1.
import { api, TRANSCRIPTION_DEFAULTS as DEF } from '../api'
import { t } from '../i18n'
import { describeError, inputCls } from '../dialogs'
import { store } from '../store'
import { refreshStatus } from '../tx'
import { esc, toast } from '../util'
import { mountSetup } from './txsetup'

type Key = keyof typeof DEF
const get = (k: Key) => store.boot.settings[k] ?? DEF[k]
const on = (k: Key) => get(k) === '1'

async function save(k: Key, v: string) {
  try {
    await api.setSetting(k, v)
    store.boot.settings[k] = v
    toast(t('call.saved'))
    if (k === 'transcription_auto') void refreshStatus()
  } catch (e) { toast(describeError(e), 'err') }
}

/** 'pt-BR' → 'pt'; o que não for reconhecido mostra o padrão */
const langValue = () => {
  const v = get('transcription_language').toLowerCase().slice(0, 2)
  return ['auto', 'pt', 'en', 'es'].includes(v) ? v : 'pt'
}

export function mountTranscriptionSettings(el: HTMLElement): () => void {
  const check = (id: Key, label: string) => `<label class="flex items-center gap-2 text-sm text-zinc-300"><input type="checkbox" data-bool="${id}" ${on(id) ? 'checked' : ''} class="accent-violet-500"> ${esc(label)}</label>`
  const num = (id: Key, label: string, min: number, max: number, step: number) =>
    `<label class="block text-sm"><span class="mb-1.5 block text-zinc-400">${esc(label)}</span>
      <input type="number" data-num="${id}" min="${min}" max="${max}" step="${step}" value="${esc(get(id))}" class="${inputCls}"></label>`
  el.innerHTML = `<h2 class="text-sm font-semibold text-white">${esc(t('settings.transcription.title'))}</h2>
    <div id="tx-setup-box" class="mt-3"></div>
    <div class="mt-4 space-y-4 rounded-2xl border border-white/10 bg-ink-900/60 p-5">
      <label class="block text-sm"><span class="mb-1.5 block text-zinc-400">${esc(t('settings.transcription.language'))}</span>
        <select id="tx-lang" class="${inputCls}">${[['auto', t('settings.transcription.language_auto')], ['pt', 'Português'], ['en', 'English'], ['es', 'Español']]
          .map(([v, n]) => `<option value="${v}" ${v === langValue() ? 'selected' : ''}>${esc(n)}</option>`).join('')}</select></label>
      ${check('transcription_auto', t('settings.transcription.auto'))}
      ${check('transcription_hotwords', t('settings.transcription.hotwords'))}
      ${check('transcription_low_priority', t('settings.transcription.low_priority'))}
      <details id="tx-advanced" class="rounded-xl border border-white/10 bg-ink-950/50 p-4">
        <summary class="cursor-pointer text-sm font-medium text-zinc-200">${esc(t('settings.transcription.advanced'))}</summary>
        <div class="mt-4 grid gap-4 min-[900px]:grid-cols-2">
          ${num('transcription_threads', t('settings.transcription.threads'), 0, 16, 1)}
          ${num('transcription_beam_size', t('settings.transcription.beam'), 1, 10, 1)}
          ${num('diarization_threshold', t('settings.transcription.threshold'), 0.1, 2, 0.05)}
          ${num('bleed_margin_db', t('settings.transcription.bleed_margin'), 0, 60, 1)}
        </div>
        <div class="mt-4">${check('bleed_filter', t('settings.transcription.bleed_filter'))}</div>
      </details>
    </div>`
  const offSetup = mountSetup(el.querySelector<HTMLElement>('#tx-setup-box')!)
  el.querySelector<HTMLSelectElement>('#tx-lang')!.addEventListener('change', e => void save('transcription_language', (e.target as HTMLSelectElement).value))
  el.querySelectorAll<HTMLInputElement>('[data-bool]').forEach(i => i.addEventListener('change', () => void save(i.dataset.bool as Key, i.checked ? '1' : '0')))
  el.querySelectorAll<HTMLInputElement>('[data-num]').forEach(i => i.addEventListener('change', () => {
    const k = i.dataset.num as Key
    const lo = Number(i.min), hi = Number(i.max)
    let v = Number(i.value)
    // vazio/inválido volta ao padrão; fora da faixa é ajustado (o núcleo também valida)
    if (i.value.trim() === '' || !Number.isFinite(v)) v = Number(DEF[k])
    v = Math.min(hi, Math.max(lo, k === 'diarization_threshold' ? v : Math.round(v)))
    i.value = String(v)
    void save(k, String(v))
  }))
  return offSetup
}
