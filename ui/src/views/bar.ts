// Mini barra (janela "bar", rota `#/bar`): ponto vermelho + cronômetro + medidores + parar/gravar + abrir app +
// esconder. Roda **sem** o shell da app (sem barra lateral, sem `loadNav`, sem toast): ver `main.ts`.
// A janela (~320x56, sem decoração, sempre no topo) só tem `core:event:default` + `start-dragging`; por isso aqui
// só se ouvem `record-state` e `record-levels`, e os comandos usados são os próprios do app (`record_*`, `bar_hide`,
// `show_main_window`). Arrastar: `data-tauri-drag-region` no contêiner e nos textos, nunca nos botões.
import { api } from '../api'
import { t } from '../i18n'
import { elapsedNow, hub, isRecording, startHub, subscribe } from '../rec'
import { describeError } from '../dialogs'
import { esc, fmtTime } from '../util'

const btn = 'flex h-8 shrink-0 items-center justify-center rounded-lg border border-white/10 bg-white/5 text-zinc-300 hover:bg-white/10 hover:text-white'

export async function renderBar(el: HTMLElement): Promise<void> {
  // fundo sólido escuro (a janela não é transparente) e nada de rolagem
  document.documentElement.style.background = '#0d0f15'
  document.body.style.cssText = 'background:#0d0f15;overflow:hidden;margin:0'
  document.getElementById('toast')?.remove()

  const mini = (id: string, label: string) => `<div class="flex items-center gap-1" title="${esc(label)}">
    <span class="w-3 text-[9px] font-semibold uppercase leading-none text-zinc-500">${id === 'mic' ? 'M' : 'S'}</span>
    <div data-m="${id}" class="relative h-1.5 w-12 overflow-hidden rounded-full bg-white/10"><div class="absolute inset-y-0 left-0 w-0 rounded-full bg-emerald-400/80"></div></div></div>`

  el.innerHTML = `<div data-tauri-drag-region id="bar" class="flex h-screen w-screen select-none items-center gap-2 overflow-hidden border border-white/10 bg-ink-900 px-2.5 text-sm text-zinc-300">
    <span data-tauri-drag-region id="dot" class="h-2.5 w-2.5 shrink-0 rounded-full bg-zinc-600"></span>
    <span data-tauri-drag-region id="elapsed" class="min-w-[3.2rem] font-mono text-sm tabular-nums text-white">00:00</span>
    <div data-tauri-drag-region id="meters" class="flex shrink-0 flex-col gap-1">${mini('mic', t('record.mic'))}${mini('sys', t('record.sys'))}</div>
    <span data-tauri-drag-region id="label" class="min-w-0 flex-1 truncate text-xs text-zinc-400"></span>
    <button id="go" type="button" class="${btn} w-8" title=""></button>
    <button id="open" type="button" title="${esc(t('bar.open'))}" aria-label="${esc(t('bar.open'))}" class="${btn} w-8"><svg viewBox="0 0 16 16" class="h-4 w-4" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M9 3h4v4M13 3 7.5 8.5M6 4H4.5A1.5 1.5 0 0 0 3 5.5v6A1.5 1.5 0 0 0 4.5 13h6a1.5 1.5 0 0 0 1.5-1.5V10"/></svg></button>
    <button id="hide" type="button" title="${esc(t('bar.hide'))}" aria-label="${esc(t('bar.hide'))}" class="${btn} w-8"><svg viewBox="0 0 16 16" class="h-4 w-4" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M4 4l8 8M12 4l-8 8"/></svg></button>
  </div>`
  const $ = <T extends HTMLElement>(s: string) => el.querySelector<T>(s)!
  const goBtn = $<HTMLButtonElement>('#go')
  let err = ''
  let errTimer: ReturnType<typeof setTimeout> | undefined
  const smooth = { mic: 0, sys: 0 }

  const SQUARE = '<span class="block h-3 w-3 rounded-[3px] bg-rose-400"></span>'
  const CIRCLE = '<span class="block h-3 w-3 rounded-full bg-rose-500"></span>'

  function paintState() {
    const rec = isRecording()
    const fin = hub.status?.finalizing.length ?? 0
    const dot = $('#dot')
    dot.className = `h-2.5 w-2.5 shrink-0 rounded-full ${rec ? 'animate-pulse bg-rose-500' : fin ? 'bg-amber-400' : 'bg-zinc-600'}`
    $('#meters').style.visibility = rec ? 'visible' : 'hidden'
    $('#meters').classList.toggle('hidden', !rec)
    goBtn.innerHTML = rec ? SQUARE : CIRCLE
    goBtn.title = goBtn.ariaLabel = rec ? t('bar.stop') : t('bar.start')
    const title = hub.status?.recording?.title
    $('#label').textContent = err || (rec ? title || '' : fin ? t('bar.finalizing') : t('bar.idle'))
    $('#label').classList.toggle('text-rose-300', !!err)
    $('#elapsed').classList.toggle('text-zinc-500', !rec)
    $('#elapsed').textContent = fmtTime(elapsedNow())
  }
  function paintLevels() {
    const lv = hub.levels?.source === 'recording' ? hub.levels : null
    for (const id of ['mic', 'sys'] as const) {
      const v = lv?.[id]
      const target = v ? Math.min(1, Math.sqrt(v.rms)) : 0
      smooth[id] = target > smooth[id] ? target : smooth[id] * 0.8 + target * 0.2
      const m = $(`[data-m="${id}"]`), fill = m.firstElementChild as HTMLElement
      fill.style.width = `${(smooth[id] * 100).toFixed(1)}%`
      // mic parado há alguns segundos: âmbar (o mesmo aviso da tela principal, em miniatura)
      const warn = id === 'mic' && !!v && v.silent_s >= 3
      fill.className = `absolute inset-y-0 left-0 rounded-full ${warn ? 'bg-amber-400/80' : 'bg-emerald-400/80'}`
      m.title = warn ? t('record.mic_silent', { s: Math.floor(v!.silent_s) }) : ''
    }
  }
  const fail = (e: unknown) => {
    err = describeError(e)
    clearTimeout(errTimer)
    errTimer = setTimeout(() => { err = ''; paintState() }, 5000)
    paintState()
  }

  goBtn.addEventListener('click', async () => {
    goBtn.disabled = true
    try { isRecording() ? await api.recordStop() : await api.recordToggle() }
    catch (e) { fail(e) }
    finally { goBtn.disabled = false }
  })
  $('#open').addEventListener('click', () => { api.showMainWindow().catch(fail) })
  $('#hide').addEventListener('click', () => { api.barHide().catch(fail) })

  subscribe('state', paintState)
  subscribe('levels', paintLevels)
  setInterval(() => { if (isRecording()) $('#elapsed').textContent = fmtTime(elapsedNow()) }, 250)
  await startHub()
  paintState()
  paintLevels()
}
