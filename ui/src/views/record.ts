// Tela de gravação na janela principal (rota `#/record`): alvo/título/falantes/idioma, dispositivos com medidores
// ao vivo (pré-visualização por `record_monitor_*` antes de gravar), iniciar/parar, edição durante a gravação
// (`record_update`, sempre com o conjunto completo: o shell SUBSTITUI o intent) e progresso da finalização.
// Regra de ouro: nunca redesenhar o formulário por evento; medidores/cronômetro/botões mudam no lugar.
import { api, type DeviceInfo, type RecordDevices, type RecordInfo, type StreamChoice, type StreamLevel } from '../api'
import { t } from '../i18n'
import { elapsedNow, hub, isRecording, progressFrac, stageLabel, startHub, subscribe } from '../rec'
import { libName, store, type View } from '../store'
import { describeError, inputCls } from '../dialogs'
import { esc, fmtClock, fmtDateShort, fmtTime } from '../util'

const SILENCE_WARN_S = 3 // mic sem áudio por tanto tempo → aviso (mudo ou dispositivo errado)

// Seleção de dispositivo ⇄ valor do <select>
const choiceToValue = (c: StreamChoice) => (typeof c === 'string' ? c : `dev:${c.named}`)
const valueToChoice = (v: string): StreamChoice => (v === 'default' || v === 'off' ? v : { named: v.slice(4) })

interface Form {
  libraryId: number; clientId: number | null; title: string; speakers: string; language: string
  mic: StreamChoice; sys: StreamChoice
}

export async function renderRecord(el: HTMLElement): Promise<View> {
  await startHub()
  const [info, devs]: [RecordInfo, RecordDevices] = await Promise.all([api.recordInfo(), api.recordDevices()])
  const inbox = store.boot.inbox_id
  const unavailable = devs.backend === 'unavailable' || info.backend === 'unavailable'
  const libs = () => store.libraries.filter(l => l.available)

  const initialForm = (): Form => {
    const r = hub.status?.recording
    if (r) {
      return {
        libraryId: r.library_id, clientId: r.client_id, title: r.title, speakers: r.expected_speakers?.toString() ?? '',
        language: r.language ?? '',
        mic: r.mic ? { named: r.mic.device } : 'off', sys: r.sys ? { named: r.sys.device } : 'off',
      }
    }
    const lu = info.last_used
    const lib = libs().find(l => l.id === lu.library_id) ?? libs().find(l => l.id === inbox)
    return { libraryId: lib?.id ?? inbox, clientId: lu.client_id, title: '', speakers: '', language: '', mic: lu.mic, sys: lu.sys }
  }
  let f = initialForm()
  let wasRecording = isRecording()
  const cleanups: (() => void)[] = []
  let updateTimer: ReturnType<typeof setTimeout> | undefined
  let updating = false
  let disposed = false

  // ---------------------------------------------------------------- HTML
  const micDevs = devs.devices.filter(d => !d.is_monitor)
  const sysDevs = devs.devices.filter(d => d.is_monitor)
  const devOpts = (list: DeviceInfo[], cur: StreamChoice) => {
    const known = typeof cur === 'string' || list.some(d => d.name === cur.named)
    return `<option value="default">${esc(t('record.dev_default'))}</option><option value="off">${esc(t('record.dev_off'))}</option>` +
      list.map(d => `<option value="dev:${esc(d.name)}">${esc(d.description || d.name)}${d.is_default ? ' · ' + esc(t('record.dev_is_default')) : ''}</option>`).join('') +
      (known ? '' : `<option value="dev:${esc((cur as { named: string }).named)}">${esc((cur as { named: string }).named)} · ${esc(t('record.dev_missing'))}</option>`)
  }
  const meter = (id: string) => `<div data-meter="${id}">
    <div class="relative h-3 overflow-hidden rounded-full bg-white/[0.06]">
      <div data-fill class="absolute inset-y-0 left-0 w-0 rounded-full bg-emerald-500/70"></div>
      <div data-peak class="absolute inset-y-0 w-0.5 bg-emerald-200" style="left:0"></div>
    </div>
    <p data-note class="mt-1.5 min-h-4 text-xs"></p></div>`
  const now = new Date()
  const nowIso = `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, '0')}-${String(now.getDate()).padStart(2, '0')}T${String(now.getHours()).padStart(2, '0')}:${String(now.getMinutes()).padStart(2, '0')}:00`
  const placeholder = t('call.untitled', { date: fmtDateShort(nowIso), time: fmtClock(nowIso) })

  el.innerHTML = `<div class="mx-auto max-w-3xl px-6 py-10">
    <div class="flex items-center justify-between gap-4">
      <h1 class="text-3xl font-semibold tracking-tight text-white">${esc(t('nav.record'))}</h1>
      <span id="state-chip" class="rounded-full bg-white/5 px-3 py-1 text-xs text-zinc-400"></span>
    </div>
    ${unavailable ? `<div id="no-backend" class="mt-6 rounded-2xl border border-rose-400/30 bg-rose-400/5 p-4 text-sm text-rose-200">${esc(t('record.no_backend'))}</div>` : ''}

    <section class="mt-6 rounded-2xl border border-white/10 bg-ink-900/60 p-5">
      <div id="clock-box" class="mb-5 hidden items-center justify-between gap-4 rounded-xl border border-rose-400/30 bg-rose-400/[0.06] px-4 py-3">
        <div class="flex items-center gap-3"><span class="h-3 w-3 animate-pulse rounded-full bg-rose-500"></span><span class="text-sm font-medium text-rose-200">${esc(t('record.recording'))}</span></div>
        <span id="elapsed" class="font-mono text-3xl tabular-nums text-white">00:00</span>
      </div>
      <h2 class="mb-3 text-sm font-semibold text-white">${esc(t('record.what'))}</h2>
      <div class="grid gap-4 sm:grid-cols-2">
        <label class="block text-sm"><span class="mb-1.5 block text-zinc-400">${esc(t('assign.company'))}</span>
          <select id="f-lib" class="${inputCls}"></select></label>
        <label id="client-wrap" class="block text-sm"><span class="mb-1.5 block text-zinc-400">${esc(t('assign.client'))}</span>
          <select id="f-client" class="${inputCls}"></select></label>
        <label class="block text-sm sm:col-span-2"><span class="mb-1.5 block text-zinc-400">${esc(t('call.title'))} <span class="text-zinc-600">· ${esc(t('record.optional'))}</span></span>
          <input id="f-title" maxlength="200" autocomplete="off" class="${inputCls}" placeholder="${esc(placeholder)}">
          <span class="mt-1 block text-xs text-zinc-600">${esc(t('call.title_hint'))}</span></label>
        <label class="block text-sm"><span class="mb-1.5 block text-zinc-400">${esc(t('record.speakers'))} <span class="text-zinc-600">· ${esc(t('record.optional'))}</span></span>
          <input id="f-speakers" type="number" min="1" max="20" inputmode="numeric" class="${inputCls}" placeholder="1–20">
          <span class="mt-1 block text-xs text-zinc-600">${esc(t('record.speakers_hint'))}</span></label>
        <label class="block text-sm"><span class="mb-1.5 block text-zinc-400">${esc(t('record.language'))}</span>
          <select id="f-lang" class="${inputCls}">
            <option value="">${esc(t('record.language_default'))}</option>
            <option value="pt">Português</option><option value="en">English</option><option value="es">Español</option></select></label>
      </div>
      <p id="update-status" class="mt-3 hidden min-h-4 text-xs text-zinc-500"></p>
    </section>

    <section class="mt-6 rounded-2xl border border-white/10 bg-ink-900/60 p-5">
      <h2 class="text-sm font-semibold text-white">${esc(t('record.devices'))}</h2>
      <div class="mt-3 grid gap-5 sm:grid-cols-2">
        <div><label class="block text-sm"><span class="mb-1.5 block text-zinc-400">${esc(t('record.mic'))}</span>
            <select id="f-mic" class="${inputCls}">${devOpts(micDevs, f.mic)}</select></label>
          <div class="mt-3">${meter('mic')}</div></div>
        <div><label class="block text-sm"><span class="mb-1.5 block text-zinc-400">${esc(t('record.sys'))}</span>
            <select id="f-sys" class="${inputCls}">${devOpts(sysDevs, f.sys)}</select></label>
          <div class="mt-3">${meter('sys')}</div></div>
      </div>
      <p id="cuts-note" class="mt-3 hidden text-xs text-amber-300"></p>
      <p id="monitor-err" class="mt-3 hidden text-xs text-rose-300"></p>
    </section>

    <div class="mt-6 flex flex-wrap items-center gap-3">
      <button id="go" type="button" class="rounded-xl border border-rose-400/60 bg-rose-600/80 px-6 py-3 text-sm font-semibold text-white hover:bg-rose-500 disabled:cursor-not-allowed disabled:opacity-40"></button>
      <p id="go-err" class="min-w-0 flex-1 text-sm text-rose-300"></p>
    </div>

    <div id="done-box" class="mt-6"></div>
    <div id="finalizing" class="mt-6"></div>

    <section class="mt-8 rounded-2xl border border-amber-400/20 bg-amber-400/[0.04] p-5 text-sm text-zinc-400">
      <h2 class="font-semibold text-amber-200">${esc(t('record.warn_title'))}</h2>
      <ul class="mt-2 list-disc space-y-1.5 pl-5">
        <li>${esc(t('record.warn_monitor'))}</li>
        <li>${esc(t('record.warn_screen'))}</li>
        <li>${esc(t('record.warn_consent'))}</li></ul></section>
  </div>`

  const $ = <T extends HTMLElement>(s: string) => el.querySelector<T>(s)!
  const selLib = $<HTMLSelectElement>('#f-lib'), selClient = $<HTMLSelectElement>('#f-client')
  const inTitle = $<HTMLInputElement>('#f-title'), inSpk = $<HTMLInputElement>('#f-speakers'), selLang = $<HTMLSelectElement>('#f-lang')
  const selMic = $<HTMLSelectElement>('#f-mic'), selSys = $<HTMLSelectElement>('#f-sys')
  const goBtn = $<HTMLButtonElement>('#go')

  // ---------------------------------------------------------------- alvo
  function fillLibs() {
    selLib.innerHTML = libs().map(l => `<option value="${l.id}">${esc(libName(l))}</option>`).join('')
    if (!libs().some(l => l.id === f.libraryId)) f.libraryId = inbox
    selLib.value = String(f.libraryId)
    fillClients()
  }
  function fillClients() {
    const lib = libs().find(l => l.id === f.libraryId)
    const company = lib?.kind === 'company'
    $('#client-wrap').hidden = !company
    const cs = company ? store.clients.get(f.libraryId) ?? [] : []
    selClient.innerHTML = `<option value="">${esc(t('assign.no_client'))}</option>` + cs.map(c => `<option value="${c.id}">${esc(c.name)}</option>`).join('')
    if (!cs.some(c => c.id === f.clientId)) f.clientId = null
    selClient.value = f.clientId == null ? '' : String(f.clientId)
  }
  function paintForm() {
    fillLibs()
    inTitle.value = f.title; inSpk.value = f.speakers; selLang.value = f.language
    selMic.innerHTML = devOpts(micDevs, f.mic); selSys.innerHTML = devOpts(sysDevs, f.sys)
    selMic.value = choiceToValue(f.mic); selSys.value = choiceToValue(f.sys)
  }

  const speakersValue = (): number | null | 'bad' => {
    const v = f.speakers.trim()
    if (!v) return null
    const n = Number(v)
    return Number.isInteger(n) && n >= 1 && n <= 20 ? n : 'bad'
  }
  const metaArgs = () => {
    const sp = speakersValue()
    return {
      // a inbox vai com o id explícito: `record_start` sem alvo significaria "último usado"
      libraryId: f.libraryId, clientId: f.clientId, title: f.title.trim() || null,
      expectedSpeakers: sp === 'bad' ? null : sp, language: f.language || null,
    }
  }

  // ---------------------------------------------------------------- modo (ocioso × gravando)
  function syncMode() {
    const rec = isRecording()
    const finalizing = hub.status?.finalizing.length ?? 0
    $('#clock-box').classList.toggle('hidden', !rec)
    $('#clock-box').classList.toggle('flex', rec)
    selMic.disabled = selSys.disabled = rec || unavailable
    goBtn.textContent = rec ? t('record.stop') : t('record.start')
    goBtn.classList.toggle('border-rose-400/60', !rec)
    goBtn.classList.toggle('bg-rose-600/80', !rec)
    goBtn.classList.toggle('border-white/20', rec)
    goBtn.classList.toggle('bg-white/10', rec)
    goBtn.disabled = unavailable || (!rec && f.mic === 'off' && f.sys === 'off')
    $('#update-status').classList.toggle('hidden', !rec)
    const chip = $('#state-chip')
    chip.textContent = rec ? t('record.recording') : finalizing ? t('record.finalizing') : t('record.idle')
    chip.className = `rounded-full px-3 py-1 text-xs ${rec ? 'bg-rose-500/15 text-rose-200' : finalizing ? 'bg-amber-400/10 text-amber-200' : 'bg-white/5 text-zinc-400'}`
    $('#go-err').textContent = !rec && f.mic === 'off' && f.sys === 'off' ? t('record.both_off') : $('#go-err').textContent
    if (rec !== wasRecording) {
      if (rec) { f = initialForm(); paintForm(); $('#done-box').innerHTML = ''; hub.lastDone = null }
      else { f.title = ''; f.speakers = ''; inTitle.value = ''; inSpk.value = ''; if (!disposed) void startMonitor() }
      wasRecording = rec
    }
    paintCuts()
    paintFinalizing()
  }

  function paintCuts() {
    const r = hub.status?.recording
    const n = (r?.mic?.cuts ?? 0) + (r?.sys?.cuts ?? 0)
    const note = $('#cuts-note')
    note.hidden = !(isRecording() && n > 0)
    note.textContent = t('record.cuts', { n })
  }

  // ---------------------------------------------------------------- finalização
  function paintFinalizing() {
    const keys = hub.status?.finalizing ?? []
    $('#finalizing').innerHTML = keys.length
      ? `<section class="rounded-2xl border border-white/10 bg-ink-900/60 p-5">
          <h2 class="text-sm font-semibold text-white">${esc(t('record.finalizing'))}</h2>
          <ul class="mt-3 space-y-3">${keys.map(k => {
            const p = hub.progress.get(k)
            const frac = p ? progressFrac(p) : 0
            return `<li><div class="flex justify-between gap-3 text-xs"><span class="truncate font-mono text-zinc-400">${esc(k)}</span><span class="text-zinc-500">${esc(stageLabel(hub.progress.get(k)))}</span></div>
              <div class="mt-1.5 h-2 overflow-hidden rounded-full bg-white/5"><div class="h-full rounded-full bg-violet-500 transition-[width]" style="width:${Math.round(frac * 100)}%"></div></div></li>`
          }).join('')}</ul></section>`
      : ''
    const d = hub.lastDone
    $('#done-box').innerHTML = d?.ok && d.call && !isRecording()
      ? `<div class="flex items-center justify-between gap-3 rounded-2xl border border-emerald-400/30 bg-emerald-400/[0.05] px-5 py-4 text-sm text-emerald-100">
          <span>${esc(t('record.done'))}</span>
          <a href="#/call/${d.call.library_id}/${d.call.call_id}" class="font-medium text-emerald-300 hover:underline">${esc(t('record.open_call'))} →</a></div>`
      : ''
  }

  // ---------------------------------------------------------------- medidores
  const smooth = { mic: { fill: 0, peak: 0 }, sys: { fill: 0, peak: 0 } }
  function paintMeter(id: 'mic' | 'sys', lv: StreamLevel | null | undefined, off: boolean) {
    const box = el.querySelector<HTMLElement>(`[data-meter="${id}"]`)!
    const fill = box.querySelector<HTMLElement>('[data-fill]')!, peak = box.querySelector<HTMLElement>('[data-peak]')!
    const note = box.querySelector<HTMLElement>('[data-note]')!
    // escala raiz-quadrada: fala normal (pico ~0.1) já se vê; ataque rápido, queda lenta
    const target = lv ? Math.min(1, Math.sqrt(lv.rms)) : 0, tp = lv ? Math.min(1, Math.sqrt(lv.peak)) : 0
    const s = smooth[id]
    s.fill = target > s.fill ? target : s.fill * 0.8 + target * 0.2
    s.peak = tp > s.peak ? tp : Math.max(tp, s.peak - 0.03)
    fill.style.width = `${(s.fill * 100).toFixed(1)}%`
    peak.style.left = `${Math.max(0, s.peak * 100 - 0.8).toFixed(1)}%`
    const clipping = !!lv && lv.peak > 0.97
    fill.className = `absolute inset-y-0 left-0 rounded-full ${clipping ? 'bg-rose-500/80' : 'bg-emerald-500/70'}`
    box.classList.toggle('opacity-40', off)
    let msg = '', cls = 'text-zinc-500'
    if (off) msg = t('record.track_off')
    else if (lv && !lv.alive) { msg = t('record.reconnecting'); cls = 'text-amber-300' }
    else if (lv && lv.silent_s >= SILENCE_WARN_S) {
      const s = Math.floor(lv.silent_s)
      if (id === 'mic') { msg = t('record.mic_silent', { s }); cls = 'text-amber-300' }
      else msg = t('record.sys_silent', { s })
    }
    note.textContent = msg
    note.className = `mt-1.5 min-h-4 text-xs ${cls}`
  }
  function paintLevels() {
    const want = isRecording() ? 'recording' : 'monitor'
    const lv = hub.levels && hub.levels.source === want ? hub.levels : null
    paintMeter('mic', lv?.mic, f.mic === 'off')
    paintMeter('sys', lv?.sys, f.sys === 'off')
  }

  // ---------------------------------------------------------------- monitor (pré-visualização)
  async function startMonitor() {
    const err = $('#monitor-err')
    err.hidden = true
    if (unavailable || isRecording() || disposed) return
    if (f.mic === 'off' && f.sys === 'off') { await stopMonitor(); return }
    try { await api.recordMonitorStart(f.mic, f.sys) }
    catch (e) {
      if ((e as { code?: string })?.code === 'already_recording') return
      err.textContent = describeError(e); err.hidden = false
    }
  }
  async function stopMonitor() {
    try { await api.recordMonitorStop() } catch { /* sem monitor ativo */ }
  }

  // ---------------------------------------------------------------- iniciar/parar/atualizar
  async function toggle() {
    const err = $('#go-err')
    err.textContent = ''
    goBtn.disabled = true
    try {
      if (isRecording()) { await api.recordStop(); return }
      if (speakersValue() === 'bad') { err.textContent = t('record.err_speakers'); return }
      await stopMonitor()
      await api.recordStart({ ...metaArgs(), mic: f.mic, sys: f.sys })
    } catch (e) {
      err.textContent = describeError(e)
      if (!isRecording()) void startMonitor()
    } finally { syncMode() }
  }

  function scheduleUpdate(now = false) {
    if (!isRecording()) return
    clearTimeout(updateTimer)
    $('#update-status').textContent = t('call.typing')
    updateTimer = setTimeout(sendUpdate, now ? 0 : 800)
  }
  async function sendUpdate() {
    if (!isRecording()) return
    if (speakersValue() === 'bad') { $('#update-status').textContent = t('record.err_speakers'); return }
    updating = true
    $('#update-status').textContent = t('call.saving')
    try { await api.recordUpdate(metaArgs()); $('#update-status').textContent = t('call.saved') }
    catch (e) { $('#update-status').textContent = t('call.save_error', { error: describeError(e) }) }
    finally { updating = false }
  }

  selLib.addEventListener('change', () => { f.libraryId = Number(selLib.value); f.clientId = null; fillClients(); scheduleUpdate(true) })
  selClient.addEventListener('change', () => { f.clientId = selClient.value ? Number(selClient.value) : null; scheduleUpdate(true) })
  inTitle.addEventListener('input', () => { f.title = inTitle.value; scheduleUpdate() })
  inTitle.addEventListener('blur', () => scheduleUpdate(true))
  inTitle.addEventListener('keydown', e => { if (e.key === 'Enter') { e.preventDefault(); inTitle.blur() } })
  inSpk.addEventListener('input', () => { f.speakers = inSpk.value; scheduleUpdate() })
  selLang.addEventListener('change', () => { f.language = selLang.value; scheduleUpdate(true) })
  const onDevice = () => {
    f.mic = valueToChoice(selMic.value); f.sys = valueToChoice(selSys.value)
    $('#go-err').textContent = ''
    syncMode()
    paintLevels()
    void startMonitor()
  }
  selMic.addEventListener('change', onDevice)
  selSys.addEventListener('change', onDevice)
  goBtn.addEventListener('click', toggle)

  // ---------------------------------------------------------------- ligação com o hub
  cleanups.push(subscribe('state', syncMode), subscribe('levels', paintLevels), subscribe('progress', paintFinalizing))
  const clock = setInterval(() => { if (isRecording()) $('#elapsed').textContent = fmtTime(elapsedNow()) }, 250)
  cleanups.push(() => clearInterval(clock))

  paintForm()
  syncMode()
  paintLevels()
  $('#elapsed').textContent = fmtTime(elapsedNow())
  void startMonitor()

  return {
    refresh: () => { fillLibs() },
    busy: () => updating || (el.contains(document.activeElement) && !!document.activeElement?.matches('input')),
    dispose: () => {
      disposed = true
      clearTimeout(updateTimer)
      cleanups.forEach(c => c())
      // sair da tela nunca para uma gravação; só derruba a pré-visualização
      if (!isRecording()) void api.recordMonitorStop().catch(() => {})
    },
  }
}
