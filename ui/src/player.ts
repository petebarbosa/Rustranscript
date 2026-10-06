// Player de áudio da tela da chamada (issue #22). O som é do Rust (PulseAudio): aqui só a barra — tocar/pausar,
// posição, velocidade, onda sonora — e o estado, que mora fora do DOM porque `call.ts` refaz o `innerHTML`
// inteiro a cada `draw()`. A barra (`el`) é um nó único, reanexado ao dock depois de cada desenho.
import { api, on, PLAYER_EVENT, type PlayerPosition } from './api'
import { t } from './i18n'
import { esc, fmtTime, toast } from './util'

export const SPEEDS = [1, 1.5, 2] as const
/** Faixas pedidas ao Rust (a onda é redesenhada por reamostragem em qualquer largura; 1 faixa ≈ 1 px de tela). */
const BUCKETS = 1500
/** Barras da onda: largura e espaço, em px de CSS. */
const BAR = 2, GAP = 1
/** Arrastar na onda manda no máximo um `seek` a cada tanto (o motor decodifica e descarta até o ponto). */
const DRAG_MS = 80
const STEP_S = 5
/** Arrastos menores que isto não viram corte (e não somem sem querer o que já existe). */
const MIN_CUT_S = 0.3
/** Quão perto (px) da ponta de um corte novo o ponteiro pega a ponta em vez de começar outro corte. */
const GRAB_PX = 7

export interface PlayerCtl {
  /** A barra, pronta para ser anexada ao dock. */
  el: HTMLElement
  /** Posição atual (s), já com o que a UI adiantou em pulos e arrastos. */
  position(): number
  playing(): boolean
  follow(): boolean
  toggle(): void
  /** Pula para `s`; `play` começa a tocar dali (clique no tempo de um trecho). */
  seek(s: number, play?: boolean): void
  /** Cortes salvos (#23): a onda os mostra hachurados. */
  setCuts(cuts: { t_start: number; t_end: number }[]): void
  /** Modo de cortar: arrastar sobre a onda marca um corte novo (um clique sem arrastar continua sendo "pular"). */
  setCutMode(on: boolean): void
  /** Cortes marcados e ainda não salvos, em segundos, sem sobreposição. */
  pending(): [number, number][]
  setPending(spans: [number, number][]): void
  /** Fecha o player no Rust; a promessa resolve quando ele soltou os arquivos. */
  dispose(): Promise<void>
}

interface Opts {
  libraryId: number
  callId: number
  duration: number
  /** A cada evento de posição (ou pulo da UI): o trecho em destaque acompanha. */
  onPosition: (s: number, playing: boolean) => void
  /** Os cortes salvos da chamada (hachurados na onda). */
  cuts: { t_start: number; t_end: number }[]
  /** O botão "Cortar áudio" da barra. */
  onCutToggle: () => void
  /** O usuário marcou/ajustou cortes na onda (a lista da tela se refaz). */
  onPending: (spans: [number, number][]) => void
}

const ICON_PLAY = '<svg viewBox="0 0 24 24" width="16" height="16" fill="currentColor" aria-hidden="true"><path d="M8 5v14l11-7z"/></svg>'
const ICON_PAUSE = '<svg viewBox="0 0 24 24" width="16" height="16" fill="currentColor" aria-hidden="true"><path d="M7 5h3.5v14H7zM13.5 5H17v14h-3.5z"/></svg>'
const ICON_CUT = '<svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><circle cx="6" cy="6" r="3"/><circle cx="6" cy="18" r="3"/><path d="M20 4 8.1 15.9M14.5 14.5 20 20M8.1 8.1 12 12"/></svg>'
const ICON_FOLLOW = '<svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M12 3v4M12 17v4M3 12h4M17 12h4"/><circle cx="12" cy="12" r="3"/></svg>'

const FOLLOW_KEY = 'player-follow'
const loadFollow = () => { try { return localStorage.getItem(FOLLOW_KEY) !== '0' } catch { return true } }
const saveFollow = (v: boolean) => { try { localStorage.setItem(FOLLOW_KEY, v ? '1' : '0') } catch { /* sem armazenamento: vale só nesta tela */ } }

/** Ladrilho diagonal para as áreas cortadas (fundo + riscas). */
function hatch(ctx: CanvasRenderingContext2D, dpr: number, bg: string, line: string): CanvasPattern | null {
  const n = Math.max(4, Math.round(7 * dpr))
  const c = document.createElement('canvas')
  c.width = c.height = n
  const g = c.getContext('2d')
  if (!g) return null
  g.fillStyle = bg
  g.fillRect(0, 0, n, n)
  g.strokeStyle = line
  g.lineWidth = Math.max(1, dpr)
  g.beginPath()
  for (const k of [-1, 0, 1]) { g.moveTo(k * n, n); g.lineTo(k * n + n, 0) }
  g.stroke()
  return ctx.createPattern(c, 'repeat')
}

/** Junta cortes que se tocam e descarta o que ficou curto demais para ser um corte de verdade. */
function tidy(v: [number, number][]): [number, number][] {
  const out: [number, number][] = []
  for (const [a, b] of [...v].map(([a, b]): [number, number] => [Math.min(a, b), Math.max(a, b)]).sort((x, y) => x[0] - y[0])) {
    const last = out[out.length - 1]
    if (last && a <= last[1]) last[1] = Math.max(last[1], b)
    else out.push([a, b])
  }
  return out.filter(([a, b]) => b - a >= MIN_CUT_S).map(([a, b]) => [Math.round(a * 1000) / 1000, Math.round(b * 1000) / 1000])
}

export function createPlayer(o: Opts): PlayerCtl {
  const speedBtn = (s: number) => `<button type="button" data-speed="${s}" aria-pressed="${s === 1}" class="rounded-lg px-2 py-1 font-mono text-xs text-zinc-400 hover:bg-white/5 hover:text-zinc-100 aria-pressed:bg-violet-500/20 aria-pressed:text-violet-200">${s}×</button>`
  const el = document.createElement('div')
  el.id = 'player'
  el.className = 'pointer-events-auto mx-auto flex max-w-6xl flex-wrap items-center gap-x-3 gap-y-2 rounded-2xl border border-white/10 bg-ink-900/95 px-4 py-2.5 shadow-2xl backdrop-blur-md'
  el.innerHTML = `
    <button type="button" data-p="toggle" title="${esc(t('player.play'))}" aria-label="${esc(t('player.play'))}" class="flex h-9 w-9 shrink-0 items-center justify-center rounded-full bg-violet-500 text-white hover:bg-violet-400 focus-visible:outline focus-visible:outline-2 focus-visible:outline-violet-300">${ICON_PLAY}</button>
    <span data-p="time" class="shrink-0 whitespace-nowrap font-mono text-xs tabular-nums text-zinc-300"></span>
    <canvas data-p="wave" role="slider" tabindex="0" aria-label="${esc(t('player.seek'))}" aria-valuemin="0" class="order-last h-10 min-w-full flex-1 cursor-pointer touch-none rounded-lg focus-visible:outline focus-visible:outline-1 focus-visible:outline-violet-400/60 @xl:order-none @xl:min-w-0"></canvas>
    <div class="ml-auto flex shrink-0 items-center gap-1 @xl:ml-0">
    <button type="button" data-p="cut" aria-pressed="false" title="${esc(t('cut.open_hint'))}" class="flex shrink-0 items-center gap-1.5 whitespace-nowrap rounded-lg px-2 py-1 text-xs text-zinc-400 hover:bg-white/5 hover:text-zinc-100 aria-pressed:bg-violet-500/20 aria-pressed:text-violet-200">${ICON_CUT}<span>${esc(t('cut.open'))}</span></button>
    <div role="group" aria-label="${esc(t('player.speed'))}" class="flex items-center">${SPEEDS.map(speedBtn).join('')}</div>
    <button type="button" data-p="follow" title="${esc(t('player.follow'))}" aria-label="${esc(t('player.follow'))}" class="shrink-0 rounded-lg p-1.5 text-zinc-500 hover:bg-white/5 hover:text-zinc-100 aria-pressed:bg-violet-500/20 aria-pressed:text-violet-200">${ICON_FOLLOW}</button></div>`
  const btn = el.querySelector<HTMLButtonElement>('[data-p="toggle"]')!
  const timeEl = el.querySelector<HTMLElement>('[data-p="time"]')!
  const wave = el.querySelector<HTMLCanvasElement>('[data-p="wave"]')!
  const followBtn = el.querySelector<HTMLButtonElement>('[data-p="follow"]')!
  const cutBtn = el.querySelector<HTMLButtonElement>('[data-p="cut"]')!
  const ctx = wave.getContext('2d')

  let duration = o.duration
  let pos = 0
  let state: PlayerPosition['state'] = 'paused'
  let speed = 1
  let peaks: number[] | null = null
  let peakMax = 1
  let follow = loadFollow()
  let dragging = false
  let lastSent = 0
  let errored = false
  let alive = true
  let cuts = o.cuts
  let cutMode = false
  let pend: [number, number][] = []
  /** Arrasto no modo de cortar: um corte novo (a partir de `anchor`) ou uma ponta (`edge`) de um pendente. */
  let cutDrag: { anchor: number; x0: number; moved: boolean; edge?: { i: number; end: 0 | 1 } } | null = null
  const long = () => duration >= 3600

  const fail = (e: unknown) => { if (alive) toast(t('player.error', { error: (e as { detail?: string })?.detail ?? String(e) }), 'err') }
  const isPlaying = () => state === 'playing'

  // ------------------------------------------------------------ onda
  /** Cor da barra de acordo com o que já tocou. Os tokens do tema ficam em CSS; aqui os mesmos valores. */
  const PLAYED = '#a99cf9', REST = '#3f4457', HEAD = '#ffffff'

  function draw() {
    if (!ctx) return
    const dpr = window.devicePixelRatio || 1
    const w = Math.round(wave.clientWidth * dpr), h = Math.round(wave.clientHeight * dpr)
    if (!w || !h) return
    if (wave.width !== w || wave.height !== h) { wave.width = w; wave.height = h }
    ctx.clearRect(0, 0, w, h)
    const frac = duration > 0 ? Math.min(1, pos / duration) : 0
    const bar = BAR * dpr, step = (BAR + GAP) * dpr, n = Math.max(1, Math.floor(w / step))
    const mid = h / 2
    for (let i = 0; i < n; i++) {
      let v = 0
      if (peaks?.length) {
        // reamostra por máximo: cada barra cobre uma fatia da onda
        const a = Math.floor(i * peaks.length / n), b = Math.max(a + 1, Math.floor((i + 1) * peaks.length / n))
        for (let k = a; k < b; k++) if (peaks[k] > v) v = peaks[k]
        v = (v / peakMax) ** 0.75
      }
      const half = Math.max(1 * dpr, v * (mid - 2 * dpr))
      ctx.fillStyle = (i * step + bar / 2) / w <= frac ? PLAYED : REST
      ctx.fillRect(i * step, mid - half, bar, half * 2)
    }
    if (duration > 0) {
      const x = (sec: number) => Math.min(w, Math.max(0, (sec / duration) * w))
      const cover = (a: number, b: number, tile: CanvasPattern | null, edge: string) => {
        const x0 = x(a), x1 = x(b)
        if (x1 - x0 < 1) return
        if (tile) { ctx.fillStyle = tile; ctx.fillRect(x0, 0, x1 - x0, h) }
        ctx.fillStyle = edge
        ctx.fillRect(x0, 0, dpr, h); ctx.fillRect(x1 - dpr, 0, dpr, h)
      }
      const saved = hatch(ctx, dpr, 'rgba(11,13,18,0.82)', '#6b7186'), fresh = hatch(ctx, dpr, 'rgba(139,92,246,0.25)', '#a99cf9')
      for (const c of cuts) cover(c.t_start, c.t_end, saved, '#6b7186')
      for (const [a, b] of pend) cover(a, b, fresh, '#a99cf9')
      // pontas arrastáveis dos cortes novos
      if (cutMode) for (const [a, b] of pend) for (const sec of [a, b]) { ctx.fillStyle = '#ffffff'; ctx.fillRect(Math.min(w - 3 * dpr, Math.max(0, x(sec) - 1.5 * dpr)), h * 0.2, 3 * dpr, h * 0.6) }
    }
    ctx.fillStyle = HEAD
    ctx.fillRect(Math.min(w - 2 * dpr, Math.max(0, frac * w - dpr)), 0, 2 * dpr, h)
  }

  function paint() {
    const icon = isPlaying() ? ICON_PAUSE : ICON_PLAY
    const label = t(isPlaying() ? 'player.pause' : 'player.play')
    if (btn.dataset.s !== String(isPlaying())) { btn.innerHTML = icon; btn.dataset.s = String(isPlaying()) }
    btn.title = label
    btn.setAttribute('aria-label', label)
    timeEl.textContent = `${fmtTime(pos, long())} / ${fmtTime(duration, long())}`
    wave.setAttribute('aria-valuemax', String(Math.round(duration)))
    wave.setAttribute('aria-valuenow', String(Math.round(pos)))
    wave.setAttribute('aria-valuetext', timeEl.textContent)
    for (const b of el.querySelectorAll<HTMLElement>('[data-speed]')) b.setAttribute('aria-pressed', String(Number(b.dataset.speed) === speed))
    followBtn.setAttribute('aria-pressed', String(follow))
    cutBtn.setAttribute('aria-pressed', String(cutMode))
    draw()
  }

  // ------------------------------------------------------------ eventos do motor
  const offEv = on<PlayerPosition>(PLAYER_EVENT, e => {
    if (!alive || e.library_id !== o.libraryId || e.call_id !== o.callId) return
    state = e.state
    speed = e.speed
    duration = e.duration_s || duration
    if (!dragging) pos = e.position_s
    if (e.state === 'error' && !errored) { errored = true; fail({ detail: e.error?.[1] ?? e.error?.[0] ?? '' }) }
    if (e.state !== 'error') errored = false
    paint()
    o.onPosition(pos, isPlaying())
  })

  // ------------------------------------------------------------ comandos
  /** Mexe na posição na hora (a resposta do motor chega em ~100 ms) e manda o pulo. */
  function jump(s: number, send = true) {
    pos = Math.min(duration, Math.max(0, s))
    paint()
    o.onPosition(pos, isPlaying())
    if (send) { lastSent = Date.now(); api.playerSeek(pos).catch(fail) }
  }

  function toggle() {
    if (isPlaying()) api.playerPause().catch(fail)
    else api.playerPlay().catch(fail)
  }

  function seek(s: number, play = false) {
    jump(s)
    if (play && !isPlaying()) api.playerPlay().catch(fail)
  }

  const fracAt = (e: PointerEvent) => {
    const r = wave.getBoundingClientRect()
    return r.width > 0 ? Math.min(1, Math.max(0, (e.clientX - r.left) / r.width)) : 0
  }

  /** Ponta de corte novo sob o ponteiro (em px da tela), se houver. */
  const edgeAt = (e: PointerEvent) => {
    const r = wave.getBoundingClientRect()
    if (!r.width || !duration) return undefined
    for (let i = 0; i < pend.length; i++) for (const end of [0, 1] as const) {
      if (Math.abs(e.clientX - (r.left + (pend[i][end] / duration) * r.width)) <= GRAB_PX) return { i, end }
    }
    return undefined
  }

  wave.addEventListener('pointerdown', e => {
    if (e.button !== 0) return
    if (cutMode) {
      wave.setPointerCapture(e.pointerId)
      cutDrag = { anchor: fracAt(e) * duration, x0: e.clientX, moved: false, edge: edgeAt(e) }
      if (cutDrag.edge) cutDrag.moved = true
      return
    }
    dragging = true
    wave.setPointerCapture(e.pointerId)
    jump(fracAt(e) * duration)
  })
  wave.addEventListener('pointermove', e => {
    if (cutDrag) {
      const s = fracAt(e) * duration
      if (cutDrag.edge) {
        const { i, end } = cutDrag.edge
        pend[i][end] = s
      } else if (cutDrag.moved || Math.abs(e.clientX - cutDrag.x0) > 4) {
        // o corte em arrasto fica sempre no fim da lista; antes do 1º movimento não existe
        if (!cutDrag.moved) { cutDrag.moved = true; pend.push([cutDrag.anchor, s]) }
        pend[pend.length - 1] = [cutDrag.anchor, s]
      }
      draw()
      return
    }
    if (cutMode) { wave.style.cursor = edgeAt(e) ? 'ew-resize' : 'crosshair'; return }
    if (!dragging) return
    const s = fracAt(e) * duration
    // visual na hora; o som segue no ritmo de DRAG_MS (e o último ponto sempre vai, no `pointerup`)
    jump(s, false)
    if (Date.now() - lastSent >= DRAG_MS) { lastSent = Date.now(); api.playerSeek(pos).catch(fail) }
  })
  /** Fim do arrasto de corte: sem movimento é um clique (pula); com movimento junta e confere os cortes novos. */
  const endCutDrag = (e: PointerEvent | null) => {
    const d = cutDrag
    cutDrag = null
    if (!d) return
    if (!d.moved) { if (e) jump(fracAt(e) * duration); return }
    pend = tidy(pend)
    draw()
    o.onPending(pend.map(([a, b]) => [a, b]))
  }
  const endDrag = (e: PointerEvent) => {
    if (cutDrag) { endCutDrag(e); return }
    if (!dragging) return
    dragging = false
    jump(fracAt(e) * duration)
  }
  wave.addEventListener('pointerup', endDrag)
  wave.addEventListener('pointercancel', () => { dragging = false; endCutDrag(null) })
  wave.addEventListener('keydown', e => {
    if (e.key !== 'ArrowLeft' && e.key !== 'ArrowRight') return
    e.preventDefault()
    jump(pos + (e.key === 'ArrowRight' ? STEP_S : -STEP_S))
  })

  el.addEventListener('click', e => {
    const target = e.target as HTMLElement
    if (target.closest('[data-p="toggle"]')) { toggle(); return }
    if (target.closest('[data-p="cut"]')) { cutBtn.blur(); o.onCutToggle(); return }
    const sp = target.closest<HTMLElement>('[data-speed]')
    if (sp) {
      const v = Number(sp.dataset.speed)
      speed = v
      sp.blur() // o foco não fica no botão: o espaço seguinte toca/pausa em vez de apertá-lo de novo
      paint()
      api.playerSpeed(v).catch(fail)
      return
    }
    if (target.closest('[data-p="follow"]')) { follow = !follow; saveFollow(follow); followBtn.blur(); paint() }
  })

  // a onda segue a largura da barra
  const ro = new ResizeObserver(() => draw())
  ro.observe(wave)

  // onda sonora: calculada uma vez no Rust (pode levar alguns segundos numa chamada longa) e guardada em cache;
  // até chegar, a barra mostra só a posição
  api.playerPeaks(o.libraryId, o.callId, BUCKETS).then(r => {
    if (!alive) return
    peaks = r.data
    peakMax = Math.max(1, ...r.data)
    if (r.duration_s > 0) duration = r.duration_s
    paint()
  }).catch(() => { /* sem onda: o resto do player funciona */ })

  paint()

  return {
    el,
    position: () => pos,
    playing: isPlaying,
    follow: () => follow,
    toggle,
    seek,
    setCuts(v) { cuts = v; draw() },
    setCutMode(on) {
      cutMode = on
      wave.style.cursor = on ? 'crosshair' : ''
      wave.setAttribute('aria-label', t(on ? 'cut.drag_label' : 'player.seek'))
      if (!on) { pend = []; cutDrag = null }
      paint()
    },
    pending: () => pend.map(([a, b]) => [a, b]),
    setPending(v) { pend = v.map(([a, b]) => [a, b]); draw() },
    dispose() {
      alive = false
      void offEv.then(f => f())
      ro.disconnect()
      el.remove()
      return api.playerClose(o.libraryId, o.callId).catch(() => {})
    },
  }
}
