import { api, toError, type BleedRemoval, type BlockEdit, type BlockInfo, type CallDetail, type Hit, type HistoryEntry, type JobInfo, type Rule, type RuleKind, type SpeakerInfo } from '../api'
import { t } from '../i18n'
import { hooks, libName, meName, store, type View } from '../store'
import { assignDialog, btnCls, confirmDialog, describeError, describeGlossaryError, field, form, inputCls, renameDialog } from '../dialogs'
import { cancelJob, enqueueCall, isReady, jobError, jobForCall, jobProgress, retryJob, stageText, subscribe as subscribeTx, tx } from '../tx'
import { errorHtml, onWhyClick, queuedWhy, whyHtml } from './txwhy'
import { barHtml, callTitle, diffWords, esc, fmtBytes, fmtClock, fmtDate, fmtDuration, fmtNumber, fmtTime, fold, speakerDefault, speakerName, toast, toastAction } from '../util'
import { caseBadge, readRule, ruleFields, ruleText, syncKind } from '../rules'
import { createPlayer, type PlayerCtl } from '../player'
import { audioError, confirmAudioDelete } from '../audio'

// (rótulo, balão) para quem não é o microfone
const PALETTE = [
  ['text-sky-300', 'border-white/10 bg-ink-800/80'],
  ['text-emerald-300', 'border-emerald-400/15 bg-emerald-400/[0.04]'],
  ['text-amber-300', 'border-amber-400/15 bg-amber-400/[0.04]'],
  ['text-rose-300', 'border-rose-400/15 bg-rose-400/[0.04]'],
]
const ME_STYLE = ['text-violet-300', 'border-violet-400/25 bg-violet-400/[0.07]']
const BUCKET = 300

interface Section { id: string; label: string; sub: string; blocks: BlockInfo[] }

function sections(d: CallDetail): Section[] {
  const out: Section[] = []
  const chs = [...d.chapters].sort((a, b) => a.t - b.t)
  // fim do último capítulo não passa da duração da chamada (sem duração: fim do último bloco) — #17
  const total = d.duration_s > 0 ? d.duration_s : Math.max(0, ...d.blocks.map(b => b.t_end))
  for (const b of d.blocks) {
    let key: number, label: string, sub = ''
    if (chs.length) {
      const owner = chs.filter(c => c.t <= b.t_start).pop()
      key = owner ? owner.t : -1
      label = owner ? owner.title : t('call.start')
      sub = fmtTime(Math.max(key, 0))
    } else {
      key = Math.floor(b.t_start / BUCKET) * BUCKET
      label = `${fmtTime(key)} – ${fmtTime(Math.max(key, Math.min(key + BUCKET, total)))}`
    }
    const last = out[out.length - 1]
    if (!last || last.id !== `s-${key}`) out.push({ id: `s-${key}`, label, sub, blocks: [] })
    out[out.length - 1].blocks.push(b)
  }
  return out
}

export async function renderCall(el: HTMLElement, libraryId: number, callId: number, params: URLSearchParams): Promise<View> {
  let d = await api.callDetail(libraryId, callId)
  /** removidos como eco na versão ativa (vazio se não há versão ou o shell não responde) */
  let bleed: BleedRemoval[] = []
  const loadBleed = async () => { bleed = d.transcript_id == null ? [] : await api.bleedRemovals(libraryId, d.transcript_id).catch(() => []) }
  await loadBleed()
  const dismissed = new Set<number>()
  let io: IntersectionObserver | null = null
  let ro: ResizeObserver | null = null
  /** Player de áudio (issue #22): vive fora do DOM, que `draw()` refaz; a barra é reanexada ao dock a cada desenho. */
  let player: PlayerCtl | null = null
  /** Por que não há player (linha discreta sob o cabeçalho); null = tem áudio, ou ainda se descobrindo. */
  let audioNote: string | null = null
  /** Modo de cortar áudio (#23): a lista de cortes aparece acima da barra do player; os cortes novos vivem no player. */
  let cutMode = false
  /** Assinatura dos cortes que o player já conhece (o que mudou depois de um `reload` precisa ser reenviado ao Rust). */
  const cutSig = () => d.cuts.map(c => `${c.t_start}-${c.t_end}`).join(',')
  let cutsSeen = cutSig()

  const speakerById = () => new Map(d.speakers.map(s => [s.id, s]))
  const styleFor = (s: SpeakerInfo | undefined, order: Map<number, number>): string[] => {
    if (!s || s.track === 'mic') return ME_STYLE
    if (!order.has(s.id)) order.set(s.id, order.size)
    return PALETTE[order.get(s.id)! % PALETTE.length]
  }
  const nameOf = (s: SpeakerInfo | undefined) => speakerName(s, meName())

  function blockHtml(b: BlockInfo, spk: Map<number, SpeakerInfo>, order: Map<number, number>) {
    const s = spk.get(b.speaker_id)
    const [label, box] = styleFor(s, order)
    const me = s?.track === 'mic'
    const edited = b.edited ? ` data-edited title="${esc(t('call.original', { text: b.original_text }))}"` : ''
    return `<article id="b-${b.id}" data-block="${b.id}"${edited} class="relative flex ${me ? 'justify-end' : 'justify-start'} group-data-[selecting]/sel:cursor-pointer group-data-[selecting]/sel:pl-9">
      <div class="relative min-w-[14rem] max-w-[46rem] rounded-2xl border ${box} px-4 py-3 shadow-sm">
        <button type="button" data-delete title="${esc(t('call.delete_block'))}" aria-label="${esc(t('call.delete_block'))}" class="absolute right-9 top-2 rounded-lg px-1.5 py-1 text-zinc-500 hover:bg-white/5 hover:text-rose-300 focus-visible:outline focus-visible:outline-1 focus-visible:outline-rose-400/60 group-data-[selecting]/sel:hidden"><svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M3 6h18M8 6V4h8v2M6 6l1 14h10l1-14M10 10v6M14 10v6"/></svg></button>
        <button type="button" data-edit title="${esc(t('call.edit_block'))}" aria-label="${esc(t('call.edit_block'))}" class="absolute right-2 top-2 rounded-lg px-1.5 py-0.5 text-sm leading-none text-zinc-500 hover:bg-white/5 hover:text-violet-300 focus-visible:outline focus-visible:outline-1 focus-visible:outline-violet-400/60 group-data-[selecting]/sel:hidden">✎</button>
        <div class="mb-1 flex items-center gap-2 pr-14 text-xs">
          <button type="button" data-speaker-btn class="font-semibold ${label}">${esc(nameOf(s))}</button>
          <a href="#/call/${libraryId}/${callId}?b=${b.id}" data-anchor class="font-mono text-zinc-500 hover:text-zinc-200">${fmtTime(b.t_start)}</a>
          <span data-badge class="hidden items-center rounded-full bg-amber-400/10 px-2 py-px text-[10px] font-medium text-amber-300">${esc(t('call.edited'))}</span>
          <button type="button" data-revert class="hidden items-center rounded-full px-2 py-px text-[10px] text-zinc-500 hover:bg-white/5 hover:text-zinc-200">↺ ${esc(t('call.revert'))}</button>
        </div>
        <p data-text class="leading-relaxed text-zinc-200">${esc(b.text)}</p>
      </div>
      <span data-check aria-hidden="true" class="absolute left-0 top-3.5 hidden h-5 w-5 items-center justify-center rounded-md border border-white/25 bg-ink-900 text-[13px] leading-none text-transparent group-data-[selecting]/sel:flex">✓</span></article>`
  }

  /** Chamada recém-gravada: sem transcrição (transcript_id null; blocos, falantes e capítulos vêm vazios). */
  const noTranscript = () => d.transcript_id == null

  const kindLabel = (j: JobInfo) => t(`queue.kind.${j.kind}`)
  const btnSm = 'rounded-lg border border-white/10 bg-ink-800 px-3 py-1.5 text-xs text-zinc-200 hover:border-violet-400/50'

  /** Estado da chamada sem transcrição: pendente / na fila / transcrevendo (etapa + barra) / falhou (com "tentar de novo"). */
  function pendingHtml() {
    const job = jobForCall(libraryId, callId)
    const k: 'pending' | 'queued' | 'running' | 'failed' = job
      ? job.state === 'running' ? 'running' : job.state === 'queued' ? 'queued' : 'failed'
      : d.transcription_state === 'done' ? 'pending' : d.transcription_state
    const auto = (store.boot.settings.transcription_auto ?? '1') === '1'
    const tracks = [
      d.audio.mic_path && t('record.track_mic'), d.audio.sys_path && t('record.track_sys'),
    ].filter(Boolean) as string[]
    const tone = k === 'failed' ? 'border-rose-400/30 bg-rose-400/[0.05]' : 'border-white/10 bg-ink-900/60'
    // chamada `failed` sem tarefa na fila (já limpa): o erro guardado na chamada é "código: detalhe"
    const stored = /^([a-z_]+): ([\s\S]*)$/.exec(d.transcription_error ?? '')
    const body = k === 'failed' ? errorHtml(job ?? { error_code: stored?.[1] ?? null, error_detail: stored ? stored[2] : d.transcription_error }, { center: true })
      : k === 'running' && job ? `<p class="mx-auto mt-2 max-w-xl text-sm text-zinc-300">${esc(stageText(job))}</p><div class="mx-auto mt-3 max-w-sm">${barHtml(jobProgress(job).fraction, 'bg-sky-400')}</div>`
      : k === 'queued' && job ? `<div class="mt-2">${whyHtml(job, { center: true })}</div>`
      : `<p class="mx-auto mt-2 max-w-xl text-sm text-zinc-400">${esc(t(k === 'pending' && !auto ? 'transcription.pending_body_manual' : `transcription.${k}_body`, { error: d.transcription_error ?? '' }))}</p>`
    const notice = (k === 'pending' && tx.status && !isReady() ? `<p class="mx-auto mt-3 max-w-xl text-xs text-amber-300">${esc(t('transcription.setup_needed'))} <a href="#/settings" class="underline hover:text-amber-100">${esc(t('transcription.open_settings'))}</a></p>` : '')
    const actions = k === 'pending' && !auto ? `<button type="button" data-tx="enqueue" class="${btnCls.btnPrimary}">${esc(t('transcription.start'))}</button>`
      : k === 'failed' ? `<button type="button" data-tx="retry" class="${btnCls.btnPrimary}">${esc(t('queue.retry'))}</button>`
      : (k === 'queued' || k === 'running') && job ? `<button type="button" data-tx="cancel" class="${btnCls.btn}">${esc(t('queue.cancel'))}</button>` : ''
    return `<div id="pending-state" data-kind="${k}" class="mt-4 rounded-2xl border ${tone} p-8 text-center">
      <p class="text-4xl text-zinc-500">${k === 'running' ? '◔' : k === 'failed' ? '⚠' : k === 'queued' ? '◷' : '◌'}</p>
      <h2 class="mt-3 text-lg font-semibold text-white">${esc(t(`transcription.${k}_title`))}</h2>
      ${body}${notice}
      ${actions ? `<div class="mt-5">${actions}</div>` : ''}
      <p class="mt-5 text-xs text-zinc-500">${d.audio.deleted_at
        ? esc(t('call.audio_deleted'))
        : tracks.length ? `${esc(t('call.audio_present'))}: ${tracks.map(x => `<span class="ml-1 rounded-full bg-white/5 px-2 py-0.5 text-zinc-300">${esc(x)}</span>`).join('')}` : esc(t('call.audio_none'))}</p>
    </div>`
  }

  /** Faixa sob o título quando há tarefa desta chamada que já tem versão (separar vozes, remontar, transcrever de novo). */
  function jobStripHtml() {
    const j = jobForCall(libraryId, callId)
    if (noTranscript() || !j || (j.state === 'failed' && dismissed.has(j.id))) return ''
    if (j.state === 'failed') {
      const e = jobError(j)
      return `<div class="flex flex-wrap items-center gap-3 rounded-xl border border-rose-400/25 bg-rose-400/[0.05] px-3 py-2 text-xs text-rose-200">
        <span class="font-medium">${esc(kindLabel(j))}</span><span class="min-w-0 flex-1">${esc(e.title)}</span>
        <button type="button" data-tx="retry" class="${btnSm}">${esc(t('queue.retry'))}</button>
        <button type="button" data-tx="dismiss" aria-label="${esc(t('common.close'))}" class="rounded-lg px-2 text-base leading-none text-rose-300 hover:bg-white/5">×</button></div>`
    }
    const running = j.state === 'running'
    return `<div class="flex flex-wrap items-center gap-3 rounded-xl border border-sky-400/20 bg-sky-400/[0.05] px-3 py-2 text-xs text-zinc-300">
      <span class="font-medium text-sky-200">${esc(kindLabel(j))}</span>
      <span class="min-w-0 flex-1" title="${esc(running ? '' : queuedWhy(j).text)}">${esc(running ? stageText(j) : queuedWhy(j).text)}</span>
      ${running ? `<div class="w-32 shrink-0">${barHtml(jobProgress(j).fraction, 'bg-sky-400')}</div>` : ''}
      <button type="button" data-tx="cancel" class="${btnSm}">${esc(t('queue.cancel'))}</button></div>`
  }

  /** Atualiza só a faixa/estado da tarefa (a cada `queue-progress`), sem refazer a página. */
  function paintJob() {
    if (noTranscript()) { const p = el.querySelector('#pending-state'); if (p) p.outerHTML = pendingHtml() }
    else { const s = el.querySelector('#job-strip'); if (s) s.innerHTML = jobStripHtml() }
  }

  function draw(keepScroll = false) {
    const pending = noTranscript()
    const scroll = el.scrollTop
    const secs = sections(d)
    const spk = speakerById()
    const order = new Map<number, number>()
    const lib = store.libraries.find(l => l.id === d.library_id)
    const place = lib?.kind === 'inbox' ? t('nav.unclassified') : [lib?.name, d.client_name ?? t('nav.no_client')].join(' · ')
    const back = lib?.kind === 'inbox' ? '#/unclassified' : d.client_id ? `#/lib/${d.library_id}/client/${d.client_id}` : `#/lib/${d.library_id}`
    const chip = (s: string) => `<span class="rounded-full border border-white/10 bg-white/[0.03] px-2.5 py-0.5 text-xs text-zinc-400">${esc(s)}</span>`
    const pill = 'shrink-0 whitespace-nowrap rounded-full border border-white/10 bg-ink-900 px-2.5 py-0.5 text-xs text-zinc-300 hover:border-violet-400/50 hover:text-white'
    const pj = pending ? jobForCall(libraryId, callId) : undefined
    const pk = pj ? (pj.state === 'running' ? 'running' : pj.state === 'queued' ? 'queued' : 'failed') : d.transcription_state === 'done' ? 'pending' : d.transcription_state
    const versions = d.transcripts.length > 1
      ? `<select id="version" class="rounded-full border border-white/10 bg-ink-900 px-2.5 py-0.5 text-xs text-zinc-300">${d.transcripts
          .map(v => `<option value="${v.id}" ${v.id === d.transcript_id ? 'selected' : ''}>${esc(t('call.version', { v: v.version }))}${v.model ? ' · ' + esc(v.model) : ''}</option>`).join('')}</select>`
      : ''
    const nav = secs.map(s => `<a href="#${s.id}" data-nav="${s.id}" class="block rounded-lg border-l-2 border-transparent px-3 py-1.5 text-sm text-zinc-400 hover:bg-white/5 hover:text-zinc-100">${esc(s.label)}${s.sub ? `<span class="block font-mono text-[11px] text-zinc-600">${esc(s.sub)}</span>` : ''}</a>`).join('')
    const body = secs.map(s => `<section id="${s.id}" data-section class="scroll-mt-[calc(var(--hdr)+3rem)]">
        <h2 class="sticky top-[var(--hdr)] z-10 -mx-2 mb-4 mt-10 flex items-center gap-3 bg-ink-950/90 px-2 py-1.5 text-xs font-semibold uppercase tracking-wider text-zinc-500 backdrop-blur-md first:mt-0"><span>${esc(s.label)}</span><span class="h-px flex-1 bg-white/10"></span></h2>
        <div class="space-y-3">${s.blocks.map(b => blockHtml(b, spk, order)).join('')}</div></section>`).join('')

    el.innerHTML = `
    <header class="sticky top-0 z-30 border-b border-white/10 bg-ink-950/85 backdrop-blur-xl max-lg:static">
      <div class="mx-auto max-w-6xl px-6 py-3">
        <a href="${back}" class="inline-flex items-center gap-1 text-xs text-zinc-500 hover:text-violet-300">← ${esc(place)}</a>
        <div class="mt-1 flex items-start gap-2">
          <h1 data-title class="min-w-0 text-xl font-semibold tracking-tight text-white sm:text-2xl ${d.title ? '' : 'italic'}">${esc(callTitle(d))}</h1>
          <button type="button" id="title-edit" title="${esc(t('call.edit_title'))}" class="mt-1 rounded-lg px-2 py-1 text-sm text-zinc-500 hover:bg-white/5 hover:text-violet-300">✎</button>
        </div>
        <div class="mt-2 flex flex-wrap items-center gap-2">
          ${chip(`${fmtDate(d.started_at)} · ${fmtClock(d.started_at)}`)}
          ${chip(fmtDuration(d.duration_s))}
          ${pending
            ? `<span class="rounded-full border px-2.5 py-0.5 text-xs ${pk === 'failed' ? 'border-rose-400/30 bg-rose-400/10 text-rose-300' : pk === 'running' ? 'border-sky-400/30 bg-sky-400/10 text-sky-300' : pk === 'queued' ? 'border-violet-400/30 bg-violet-400/10 text-violet-300' : 'border-amber-400/30 bg-amber-400/10 text-amber-300'}">${esc(t(`transcription.${pk}`))}</span>`
            : chip(t('list.words', { n: d.words, count: fmtNumber(d.words) }))}
          ${versions}
          ${pending ? '' : `<span class="ml-auto flex max-w-full flex-wrap items-center justify-end gap-1.5">
            <button type="button" id="speakers-btn" class="${pill}">${esc(t('call.speakers'))}</button>
            ${bleed.length ? `<button type="button" id="bleed-btn" class="${pill}">${esc(t('call.bleed_title', { n: bleed.length }))}</button>` : ''}
            <button type="button" id="redo-btn" ${d.audio.deleted_at ? `disabled title="${esc(t('audio.redo_off'))}"` : ''} class="${pill}${d.audio.deleted_at ? ' cursor-not-allowed opacity-40' : ''}">${esc(t('call.redo'))}</button>
            ${d.has_audio ? `<button type="button" id="audio-delete-btn" class="${pill} hover:!border-rose-400/50 hover:!text-rose-200">${esc(t('audio.delete'))}</button>` : ''}</span>`}
        </div>
        <div id="job-strip" class="mt-2 empty:hidden">${jobStripHtml()}</div>
        ${pending ? '' : audioNoteHtml()}
        <div class="mt-3 flex flex-wrap items-center gap-2">
          <div class="relative min-w-[14rem] flex-1" ${pending ? 'hidden' : ''}>
            <input id="q" type="search" autocomplete="off" placeholder="${esc(t('call.search'))}"
              class="w-full rounded-xl border border-white/10 bg-ink-900 py-2 pl-4 pr-4 not-placeholder-shown:pr-28 text-sm text-zinc-100 placeholder:text-zinc-600 focus:border-violet-400/60 focus:outline-none focus:ring-2 focus:ring-violet-400/20">
            <span id="count" class="pointer-events-none absolute right-3 top-2 text-xs text-zinc-500"></span>
          </div>
          <button id="assign" type="button" class="shrink-0 whitespace-nowrap rounded-xl border border-white/10 bg-ink-900 px-3 py-2 text-sm text-zinc-300 hover:border-violet-400/50">${esc(t('call.assign'))}</button>
          <button id="glossary-btn" type="button" title="${esc(t('glossary.open_hint'))}" class="shrink-0 whitespace-nowrap rounded-xl border border-white/10 bg-ink-900 px-3 py-2 text-sm text-zinc-300 hover:border-violet-400/50">${esc(t('glossary.open'))}</button>
          <button id="select-btn" type="button" ${pending ? 'hidden' : ''} aria-pressed="false" title="${esc(t('call.select_hint'))}" class="shrink-0 whitespace-nowrap rounded-xl border border-white/10 bg-ink-900 px-3 py-2 text-sm text-zinc-300 hover:border-violet-400/50">${esc(t('call.select'))}</button>
          <button id="history" type="button" class="shrink-0 whitespace-nowrap rounded-xl border border-white/10 bg-ink-900 px-3 py-2 text-sm text-zinc-300 hover:border-violet-400/50">${esc(t('call.history'))}</button>
        </div>
        <div class="mt-1.5 flex min-h-4 items-center justify-between gap-3 text-xs">
          <span id="save-status" class="ml-auto"></span>
        </div>
      </div>
    </header>
    <div class="mx-auto grid max-w-6xl gap-8 px-6 py-8 ${pending ? '' : 'lg:grid-cols-[14rem_1fr]'}">
      <aside ${pending ? 'hidden' : ''} class="hidden lg:sticky lg:top-[calc(var(--hdr)+1rem)] lg:block lg:self-start">
        <nav class="max-h-[calc(100vh-var(--hdr)-2rem)] space-y-0.5 overflow-y-auto pr-2">
          <p class="px-3 pb-2 text-[11px] font-semibold uppercase tracking-wider text-zinc-600">${esc(t('call.nav'))}</p>${nav}</nav>
      </aside>
      <main id="blocks" class="group/sel min-w-0">${pending ? pendingHtml() : `${body}
        <p class="mt-14 text-center text-xs text-zinc-700">${esc(t('call.end'))}</p>`}</main>
    </div>
    <div id="dock" class="@container pointer-events-none sticky bottom-0 z-30 px-6 pb-4">
    ${pending ? '' : cutPanelHtml()}
    ${pending ? '' : `<div id="select-bar" hidden class="pointer-events-auto mx-auto mb-3 flex w-fit max-w-[calc(100%-2rem)] flex-wrap items-center justify-center gap-x-3 gap-y-2 rounded-2xl border border-violet-400/30 bg-ink-900/95 px-4 py-2.5 shadow-2xl backdrop-blur-md">
      <span id="select-count" aria-live="polite" class="min-w-[6.5rem] text-sm font-medium text-white"></span>
      <button type="button" data-sel="all" class="rounded-lg px-2 py-1 text-xs text-zinc-400 hover:bg-white/5 hover:text-zinc-100">${esc(t('select.all'))}</button>
      <button type="button" data-sel="none" class="rounded-lg px-2 py-1 text-xs text-zinc-400 hover:bg-white/5 hover:text-zinc-100">${esc(t('select.none'))}</button>
      <button type="button" id="select-delete" data-sel="delete" class="rounded-xl border border-rose-400/50 bg-rose-500/15 px-4 py-2 text-sm font-medium text-rose-100 hover:bg-rose-500/25 disabled:cursor-not-allowed disabled:opacity-40">${esc(t('delete.action'))}</button>
      <button type="button" data-sel="cancel" class="${btnCls.btn}">${esc(t('common.cancel'))}</button></div>`}</div>`
    if (player) el.querySelector('#dock')!.append(player.el)
    ro?.disconnect()
    const header = el.querySelector('header')!
    const syncHeader = () => el.style.setProperty('--hdr', getComputedStyle(header).position === 'sticky' ? `${header.offsetHeight}px` : '0px')
    syncHeader()
    ro = new ResizeObserver(syncHeader)
    ro.observe(header); ro.observe(el)
    bind()
    paintSelect()
    indexStarts()
    paintPlaying(false)
    if (keepScroll) el.scrollTop = scroll
  }

  // ------------------------------------------------------------ comportamento
  const $ = <T extends HTMLElement>(sel: string) => el.querySelector<T>(sel)!
  const blocks = () => [...el.querySelectorAll<HTMLElement>('[data-block]')]
  const txt = (b: HTMLElement) => b.querySelector<HTMLElement>('[data-text]')!
  const blockData = (b: HTMLElement) => d.blocks.find(x => x.id === Number(b.dataset.block))!
  const setStatus = (text: string, s = '') => { const st = el.querySelector<HTMLElement>('#save-status'); if (st) { st.textContent = text; st.dataset.s = s } }
  const searching = () => !!el.querySelector<HTMLInputElement>('#q')?.value.trim()
  const norm = (s: string) => s.replace(/\s+/g, ' ').trim()

  function runSearch() {
    const q = $<HTMLInputElement>('#q'), v = q.value.trim()
    const fv = fold(v)
    let n = 0
    for (const b of blocks()) {
      const p = txt(b), cur = blockData(b).text
      if (b.hasAttribute('data-editing')) continue // editor aberto: não mexe no texto
      if (!v) { b.hidden = false; p.textContent = cur; continue }
      // busca sem acento: compara na forma "dobrada" e destaca no texto original
      const hit = fold(cur).includes(fv)
      b.hidden = !hit
      if (hit) {
        n++
        const folded = fold(cur)
        let html = '', i = 0, at: number
        while ((at = folded.indexOf(fv, i)) !== -1) {
          html += esc(cur.slice(i, at)) + '<mark>' + esc(cur.slice(at, at + v.length)) + '</mark>'
          i = at + v.length
        }
        p.innerHTML = html + esc(cur.slice(i))
      }
    }
    el.querySelectorAll<HTMLElement>('[data-section]').forEach(s => {
      const vis = !!s.querySelector('[data-block]:not([hidden])')
      s.hidden = !vis
      el.querySelectorAll<HTMLElement>(`[data-nav="${s.id}"]`).forEach(a => (a.hidden = !vis))
    })
    $('#count').textContent = v ? t('call.matches', { n }) : ''
    // o que a busca esconde sai da seleção: só se exclui o que se vê
    if (selecting) { for (const b of blocks()) if (b.hidden) selected.delete(idOf(b)); paintSelect() }
  }

  // ------------------------------------------------------------ edição por trecho
  /** Trecho em edição (um por vez): o texto salvo no momento em que o editor abriu. */
  let editor: { id: number } | null = null
  const editorOf = (b: HTMLElement) => b.querySelector<HTMLTextAreaElement>('[data-editor] textarea')
  const openBlock = () => (editor ? el.querySelector<HTMLElement>(`[data-block="${editor.id}"]`) : null)
  const isDirty = (b: HTMLElement) => { const ta = editorOf(b); return !!ta && norm(ta.value) !== blockData(b).text }

  function growEditor(ta: HTMLTextAreaElement) { ta.style.height = 'auto'; ta.style.height = `${ta.scrollHeight}px` }

  /** Abre o editor só neste trecho; os outros continuam somente leitura. */
  function openEditor(b: HTMLElement, draft?: string) {
    const cur = openBlock()
    if (cur && cur !== b) {
      if (isDirty(cur)) { editorOf(cur)?.focus(); setStatus(t('call.finish_first'), 'busy'); return }
      closeEditor(cur)
    }
    if (editor?.id === blockData(b).id) { editorOf(b)?.focus(); return }
    const info = blockData(b)
    editor = { id: info.id }
    b.setAttribute('data-editing', '')
    const p = txt(b)
    p.hidden = true
    p.insertAdjacentHTML('afterend', `<div data-editor class="mt-1">
      <textarea rows="1" maxlength="20000" spellcheck="false" aria-label="${esc(t('call.edit_block'))}" class="${inputCls} resize-none overflow-hidden leading-relaxed"></textarea>
      <div class="mt-2 flex flex-wrap items-center gap-2">
        <button type="button" data-save class="${btnCls.btnPrimary} !px-3 !py-1.5">${esc(t('common.save'))}</button>
        <button type="button" data-cancel class="${btnCls.btn} !px-3 !py-1.5">${esc(t('common.cancel'))}</button>
      </div>
      <p class="mt-1.5 text-[11px] text-zinc-600">${esc(t('call.edit_hint'))}</p></div>`)
    const ta = editorOf(b)!
    ta.value = draft ?? info.text
    growEditor(ta)
    syncEditor(b)
    ta.focus()
    ta.setSelectionRange(ta.value.length, ta.value.length)
  }

  /** Habilita Salvar conforme o rascunho. */
  function syncEditor(b: HTMLElement) {
    b.querySelector<HTMLButtonElement>('[data-save]')!.disabled = !norm(editorOf(b)!.value)
  }

  function closeEditor(b: HTMLElement) {
    b.querySelector('[data-editor]')?.remove()
    b.removeAttribute('data-editing')
    txt(b).hidden = false
    if (editor?.id === Number(b.dataset.block)) editor = null
    if (searching()) runSearch()
  }

  async function saveEditor(b: HTMLElement) {
    const ta = editorOf(b)
    if (!ta) return
    const v = norm(ta.value)
    if (!v) return
    if (v === blockData(b).text) { closeEditor(b); return }
    if (await save(b, () => api.setBlockText(libraryId, blockData(b).id, v))) closeEditor(b)
  }

  function cancelEditor(b: HTMLElement) { closeEditor(b); setStatus('') }

  function applyBlock(b: HTMLElement, info: BlockInfo) {
    const i = d.blocks.findIndex(x => x.id === info.id)
    d.blocks[i] = info
    const p = txt(b)
    p.textContent = info.text
    b.toggleAttribute('data-edited', info.edited)
    if (info.edited) b.title = t('call.original', { text: info.original_text })
    else b.removeAttribute('title')
  }

  /** Salva (histórico/desfazer pelo mesmo caminho de sempre); devolve null se falhou. */
  let lastSave: Promise<unknown> = Promise.resolve()
  function save(b: HTMLElement, fn: () => Promise<BlockInfo | BlockEdit>): Promise<BlockEdit | null> {
    setStatus(t('call.saving'), 'busy')
    const run = (async () => {
      try {
        const r = await fn() as BlockEdit
        const { edit_id: _e, suggestions: _s, ...info } = r
        applyBlock(b, info)
        setStatus(t('call.saved'), 'ok')
        return r
      } catch (e) { setStatus(t('call.save_error', { error: describeError(e) }), 'err'); return null }
    })()
    lastSave = run
    return run
  }

  /** Ao sair da tela/do app: o rascunho aberto com mudanças é salvo (nada digitado se perde). Falhou: fica, com o erro. */
  async function leave(): Promise<boolean> {
    await lastSave // um salvamento em curso (Enter/Salvar) termina antes
    const b = openBlock()
    if (!b || !editorOf(b)) return true
    if (!isDirty(b)) { closeEditor(b); return true }
    const v = norm(editorOf(b)!.value)
    if (!v) { closeEditor(b); return true } // esvaziado: nada válido a salvar
    const r = await save(b, () => api.setBlockText(libraryId, blockData(b).id, v))
    if (!r) { editorOf(b)?.focus(); return false }
    closeEditor(b)
    return true
  }

  async function reload(keepScroll = true) {
    // um editor aberto sobrevive ao redesenho (ex.: trocar o falante pelo editor)
    const ob = openBlock()
    const keep = ob ? { id: editor!.id, draft: editorOf(ob)?.value } : null
    d = await api.callDetail(libraryId, callId)
    await loadBleed()
    editor = null
    syncAudio()
    syncCuts()
    draw(keepScroll)
    runSearch()
    const kb = keep && el.querySelector<HTMLElement>(`[data-block="${keep.id}"]`)
    if (kb) openEditor(kb, keep.draft)
  }

  async function undo() {
    try {
      const e = await api.undo(libraryId, callId)
      if (!e) { toast(t('history.nothing')); return }
      await reload()
      toast(t('history.undone', { what: describeEntry(e) }))
      if (e.entity === 'call_title' || e.entity === 'speaker_name') hooks.reloadNav()
    } catch (e) { toast(describeError(e), 'err') }
  }

  function describeEntry(e: HistoryEntry) {
    if (e.batch_kind === 'delete') return t('delete.done', { n: e.batch_size ?? 1 })
    if (e.batch_kind === 'restore') return t('delete.restored', { n: e.batch_size ?? 1 })
    if (e.batch_kind === 'cut_add') return t('cut.history_add')
    if (e.batch_kind === 'cut_remove') return t('cut.history_remove')
    if (e.batch_id) return t(e.origin === 'import' ? 'history.glossary_import' : 'history.glossary_batch', { n: e.batch_size ?? 1 })
    const spk = speakerById()
    const blk = d.blocks.find(b => b.id === e.entity_id)
    switch (e.entity) {
      case 'block_text': return t('history.block_text', { seq: blk ? `#${blk.seq}` : '' })
      case 'block_speaker': return t('history.block_speaker', { seq: blk ? `#${blk.seq}` : '', from: nameOf(spk.get(Number(e.old_value))), to: nameOf(spk.get(Number(e.new_value))) })
      case 'call_title': return t('history.call_title')
      case 'speaker_name': return t('history.speaker_name', { to: e.new_value ?? t('history.cleared') })
      case 'block_deleted': return t(e.new_value ? 'delete.done' : 'delete.restored', { n: 1 })
      case 'audio_cut': return t(e.new_value ? 'cut.history_remove' : 'cut.history_add')
    }
  }

  async function showHistory() {
    // um lote (glossário aplicado) vira uma linha só
    const seenBatch = new Set<number>()
    const list = (await api.history(libraryId, callId)).filter(e => !e.batch_id || (!seenBatch.has(e.batch_id) && !!seenBatch.add(e.batch_id)))
    const short = (s: string | null) => esc((s ?? '∅').length > 120 ? (s ?? '').slice(0, 120) + '…' : s ?? '∅')
    const rows = list.map(e => `<li class="rounded-xl border border-white/10 bg-ink-950/60 px-3 py-2 ${e.undone_at ? 'opacity-50' : ''}">
      <div class="flex items-center justify-between gap-2 text-xs text-zinc-500">
        <span class="font-medium text-zinc-300">${esc(describeEntry(e))}</span>
        <span>${esc(t(`origin.${e.origin}`))} · ${esc(fmtDate(e.at))} ${esc(fmtClock(e.at))}${e.undone_at ? ' · ' + esc(t('history.undone_tag')) : ''}</span></div>
      ${e.entity === 'block_speaker' || e.batch_id ? '' : `<div class="mt-1 text-xs"><del class="text-rose-300/70">${short(e.old_value)}</del><br><ins class="text-emerald-300/80 no-underline">${short(e.new_value)}</ins></div>`}
      </li>`).join('')
    const r = await form(t('call.history'),
      list.length ? `<ul class="max-h-[60vh] space-y-2 overflow-y-auto">${rows}</ul><p class="text-xs text-zinc-600">${esc(t('history.hint'))}</p>` : `<p class="text-sm text-zinc-500">${esc(t('history.empty'))}</p>`,
      t('history.undo_last'), async () => true)
    if (r) await undo()
  }

  async function speakerMenu(b: HTMLElement) {
    const block = blockData(b)
    const spk = speakerById().get(block.speaker_id)
    const others = d.speakers.filter(s => s.id !== block.speaker_id)
    const body = `
      <label class="block text-sm"><span class="mb-1.5 block text-zinc-400">${esc(t('speaker.rename', { label: speakerDefault(spk, meName()) }))}</span>
        <input name="name" maxlength="80" class="${inputCls}" value="${esc(spk?.name ?? '')}" placeholder="${esc(nameOf(spk))}">
        <span class="mt-1 block text-xs text-zinc-600">${esc(t('speaker.rename_hint'))}</span></label>
      ${others.length ? `<label class="block text-sm"><span class="mb-1.5 block text-zinc-400">${esc(t('speaker.reassign', { seq: block.seq }))}</span>
        <select name="move" class="${inputCls}"><option value="">${esc(t('speaker.keep'))}</option>${others.map(s => `<option value="${s.id}">${esc(nameOf(s))}${s.name ? ` (${esc(s.label)})` : ''}</option>`).join('')}</select></label>` : ''}`
    const r = await form(nameOf(spk), body, t('common.save'), async f => {
      const fd = new FormData(f)
      const name = String(fd.get('name') ?? '').trim() || null
      const move = fd.get('move') ? Number(fd.get('move')) : null
      if (spk && name !== (spk.name ?? null)) await api.renameSpeaker(libraryId, spk.id, name)
      if (move) await api.setBlockSpeaker(libraryId, block.id, move)
      return true
    })
    if (r) await reload()
  }

  async function editTitle() {
    const v = await renameDialog(t('call.edit_title'), d.title, t('call.title'), true, esc(t('call.title_hint')))
    if (v === null) return
    try { await api.setTitle(libraryId, callId, v); await reload(); hooks.reloadNav() }
    catch (e) { toast(describeError(e), 'err') }
  }

  /** `back`: veio do modal do glossário (chamada sem cliente); volta a ele, na chamada já classificada ou se desistir. */
  async function assign(back = false) {
    const r = await assignDialog({ library_id: d.library_id, client_id: d.client_id })
    if (!r) { if (back) await glossaryDialog(); return }
    try {
      const moved = await api.assign(libraryId, callId, r.libraryId, r.clientId)
      const target = store.libraries.find(l => l.id === moved.library_id)
      await hooks.reloadNav()
      toast(t('assign.done', { place: target ? libName(target) : '' }))
      location.hash = `#/call/${moved.library_id}/${moved.call_id}${back ? '?glossary=1' : ''}`
    } catch (e) { toast(describeError(e), 'err') }
  }

  // ------------------------------------------------------------ seleção e exclusão
  // Exclusão lógica: o bloco some da tela e da busca, mas texto, seq e áudio ficam; "Desfazer" (e o histórico) restauram.
  let selecting = false
  const selected = new Set<number>()
  let anchor: number | null = null
  let alive = true
  const idOf = (b: HTMLElement) => Number(b.dataset.block)
  const visibleBlocks = () => blocks().filter(b => !b.hidden)
  const CHECK_ON = ['border-violet-400', 'bg-violet-500', 'text-white']
  const CHECK_OFF = ['border-white/25', 'bg-ink-900', 'text-transparent']

  /** Reflete `selecting`/`selected` na tela (caixas, realce, barra de ações, botão do cabeçalho). Roda a cada `draw`. */
  function paintSelect() {
    const main = el.querySelector<HTMLElement>('#blocks')
    main?.toggleAttribute('data-selecting', selecting)
    main?.classList.toggle('select-none', selecting)
    for (const b of blocks()) {
      const on = selected.has(idOf(b))
      b.toggleAttribute('data-selected', on)
      if (selecting) { b.setAttribute('role', 'checkbox'); b.setAttribute('aria-checked', String(on)); b.setAttribute('aria-label', t('call.select_message', { seq: blockData(b).seq })); b.tabIndex = 0 }
      else for (const a of ['role', 'aria-checked', 'aria-label', 'tabindex']) b.removeAttribute(a)
      const bubble = b.firstElementChild as HTMLElement
      bubble.classList.toggle('ring-2', on)
      bubble.classList.toggle('ring-violet-400/60', on)
      const chk = b.querySelector<HTMLElement>('[data-check]')
      chk?.classList.remove(...(on ? CHECK_OFF : CHECK_ON))
      chk?.classList.add(...(on ? CHECK_ON : CHECK_OFF))
    }
    const bar = el.querySelector<HTMLElement>('#select-bar')
    if (bar) {
      bar.hidden = !selecting
      el.querySelector('#select-count')!.textContent = t('select.count', { n: selected.size })
      el.querySelector<HTMLButtonElement>('#select-delete')!.disabled = !selected.size
    }
    const btn = el.querySelector<HTMLButtonElement>('#select-btn')
    if (btn) {
      btn.setAttribute('aria-pressed', String(selecting))
      btn.classList.toggle('border-violet-400/60', selecting)
      btn.classList.toggle('border-white/10', !selecting)
      btn.classList.toggle('text-white', selecting)
      btn.classList.toggle('text-zinc-300', !selecting)
    }
  }

  /** Liga/desliga o modo. Ao ligar, um editor aberto é salvo antes (mesmo caminho do `leave`); se falhar, o modo não liga. */
  async function setSelecting(on: boolean) {
    if (on === selecting || noTranscript()) return
    if (on && !(await leave())) return
    selecting = on
    if (!on) { selected.clear(); anchor = null }
    paintSelect()
  }

  /** Clique marca/desmarca; Shift+clique marca o intervalo (só os trechos visíveis) desde o último clique. */
  function toggleSelect(b: HTMLElement, range: boolean) {
    const id = idOf(b)
    const vis = visibleBlocks()
    const i = anchor == null ? -1 : vis.findIndex(x => idOf(x) === anchor), j = vis.indexOf(b)
    if (range && i >= 0 && j >= 0) vis.slice(Math.min(i, j), Math.max(i, j) + 1).forEach(x => selected.add(idOf(x)))
    else if (selected.has(id)) selected.delete(id)
    else selected.add(id)
    anchor = id
    paintSelect()
  }

  function selectAll(on: boolean) {
    selected.clear()
    if (on) visibleBlocks().forEach(b => selected.add(idOf(b)))
    anchor = null
    paintSelect()
  }

  /** Confirma (com a contagem), exclui num lote só e oferece "Desfazer". */
  async function deleteBlocks(ids: number[]) {
    if (!ids.length) return
    if (!(await confirmDialog(t('delete.confirm_title', { n: ids.length }), t('delete.confirm_body'), t('delete.action')))) return
    // o editor aberto numa mensagem que vai embora fecha sem salvar: salvar depois (leave/autosave) seria editar uma mensagem excluída
    const ob = openBlock()
    if (ob && ids.includes(idOf(ob))) closeEditor(ob)
    let done: number[]
    try { done = (await api.deleteBlocks(libraryId, ids)).changed.map(b => b.id) }
    catch (e) { toast(describeError(e), 'err'); return }
    selected.clear(); anchor = null; selecting = false
    await reload()
    if (done.length) toastAction(t('delete.done', { n: done.length }), t('delete.undo'), () => void restoreBlocks(done))
  }

  async function restoreBlocks(ids: number[]) {
    try {
      const r = await api.restoreBlocks(libraryId, ids)
      if (alive) await reload()
      toast(t('delete.restored', { n: r.changed.length }))
    } catch (e) { toast(describeError(e), 'err') }
  }

  // ------------------------------------------------------------ falantes, refazer, eco removido
  /** Renomeia vários falantes de uma vez; vazio volta ao rótulo original. */
  async function speakersDialog() {
    const ordered = [...d.speakers].sort((a, b) => Number(b.track === 'mic') - Number(a.track === 'mic') || a.label.localeCompare(b.label, undefined, { numeric: true }))
    const rows = ordered.map(s => field(
      `${speakerDefault(s, meName())} · ${t(s.track === 'mic' ? 'record.track_mic' : 'record.track_sys')}`,
      `<input name="spk-${s.id}" maxlength="80" class="${inputCls}" value="${esc(s.name ?? '')}" placeholder="${esc(speakerDefault(s, meName()))}">`,
    )).join('')
    const r = await form(t('call.speakers_title'), `${rows}<p class="text-xs text-zinc-600">${esc(t('speaker.rename_hint'))}</p>`, t('common.save'), async f => {
      const fd = new FormData(f)
      let n = 0
      for (const s of d.speakers) {
        const name = String(fd.get(`spk-${s.id}`) ?? '').trim() || null
        if (name !== (s.name ?? null)) { await api.renameSpeaker(libraryId, s.id, name); n++ }
      }
      return n
    })
    if (r) { await reload(); toast(t('call.saved')) }
  }

  /** Refazer: separar vozes (nº de pessoas do outro lado), remontar, remontar sem o filtro de eco, ou transcrever tudo de novo. */
  async function redoDialog() {
    const hasRaw = d.transcripts.find(v => v.id === d.transcript_id)?.has_raw ?? false
    const opt = (value: string, label: string, hint: string, extra = '', off = false) => `<label class="flex items-start gap-3 rounded-xl border border-white/10 bg-ink-950/50 p-3 text-sm ${off ? 'opacity-50' : 'cursor-pointer hover:border-violet-400/40'}">
      <input type="radio" name="kind" value="${value}" ${off ? 'disabled' : ''} class="mt-1 accent-violet-500">
      <span class="min-w-0 flex-1"><span class="block font-medium text-zinc-100">${esc(label)}</span><span class="mt-0.5 block text-xs text-zinc-500">${esc(hint)}</span>${extra}</span></label>`
    const body = `${hasRaw ? '' : `<p class="rounded-xl border border-amber-400/20 bg-amber-400/[0.04] px-3 py-2 text-xs text-amber-200">${esc(t('call.no_raw'))}</p>`}
      ${opt('rediarize', t('call.rediarize'), t('call.rediarize_hint'),
        `<span class="mt-2 block text-xs text-zinc-400">${esc(t('call.rediarize_speakers'))}<input type="number" name="speakers" min="1" max="20" step="1" value="${d.expected_speakers ?? ''}" class="${inputCls} mt-1 !w-28"></span>`, !hasRaw)}
      ${opt('resegment', t('call.resegment'), t('call.resegment_hint'), '', !hasRaw)}
      ${opt('nobleed', t('call.resegment_nobleed'), t('call.resegment_nobleed_hint'), '', !hasRaw)}
      ${opt('full', t('call.retranscribe'), t('call.retranscribe_hint'))}`
    const r = await form(t('call.redo_title'), body, t('call.redo_ok'), async f => {
      const fd = new FormData(f)
      const kind = String(fd.get('kind') ?? '')
      if (!kind) throw { code: 'invalid', detail: t('call.redo_pick') }
      const n = Number(fd.get('speakers'))
      try {
        if (kind === 'rediarize') await api.transcribeEnqueue(libraryId, callId, 'rediarize', n >= 1 ? { expected_speakers: Math.min(20, Math.round(n)) } : null)
        else if (kind === 'resegment') await api.transcribeEnqueue(libraryId, callId, 'resegment', null)
        else if (kind === 'nobleed') await api.transcribeEnqueue(libraryId, callId, 'resegment', { bleed_filter: false })
        else await api.transcribeEnqueue(libraryId, callId, 'full', null)
      } catch (e) {
        throw toError(e).code === 'conflict' ? { code: 'conflict', detail: t('queue.already_open') } : e
      }
      return true
    }, f => {
      const first = f.querySelector<HTMLInputElement>('input[name="kind"]:not([disabled])')
      if (first) first.checked = true
      // digitar o nº de pessoas escolhe "separar vozes"
      f.querySelector('input[name="speakers"]')?.addEventListener('focus', () => { (f.querySelector('input[value="rediarize"]') as HTMLInputElement).checked = true })
    })
    if (r) { toast(t('queue.enqueued')); paintJob() }
  }

  /** Apagar o áudio (irreversível): a confirmação mostra o tamanho que será liberado; a transcrição fica. */
  async function deleteAudioDialog() {
    let plan
    try { plan = await api.audioDelete(libraryId, callId, true) } catch (e) { toast(audioError(e), 'err'); return }
    const r = await confirmAudioDelete(t('audio.delete_title'), plan.bytes, async () => {
      // o player tem os FLACs abertos: fecha (e espera) antes de apagar; se a exclusão falhar, ele volta
      if (player) { const p = player; player = null; await p.dispose() }
      try { return await api.audioDelete(libraryId, callId, false) }
      catch (e) { void startPlayer(); throw e }
    })
    if (!r) return
    await reload()
    toast(t('audio.deleted_toast', { size: fmtBytes(r.bytes) }))
  }

  /** Trechos do microfone descartados como eco. Restaurar = remontar sem o filtro (o bruto é preservado). */
  async function bleedDialog() {
    const reason = (r: BleedRemoval) => t(`call.bleed_reason.${r.reason}`)
    const rows = bleed.map(r => `<li class="rounded-xl border border-white/10 bg-ink-950/60 px-3 py-2">
      <div class="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-zinc-500">
        <span class="font-mono text-zinc-300">${fmtTime(r.t_start)}–${fmtTime(r.t_end)}</span><span>${esc(reason(r))}</span>
        ${r.containment != null ? `<span>${Math.round(r.containment * 100)}%</span>` : ''}${r.margin_db != null ? `<span>${r.margin_db.toFixed(1)} dB</span>` : ''}</div>
      <p class="mt-1 text-sm leading-relaxed text-zinc-300">${esc(r.text)}</p></li>`).join('')
    const hasRaw = d.transcripts.find(v => v.id === d.transcript_id)?.has_raw ?? false
    const r = await form(t('call.bleed_title', { n: bleed.length }),
      `<p class="text-xs text-zinc-500">${esc(t('call.bleed_hint'))}</p><ul class="max-h-[50vh] space-y-2 overflow-y-auto">${rows}</ul>${hasRaw ? '' : `<p class="text-xs text-amber-300">${esc(t('call.no_raw'))}</p>`}`,
      t('call.resegment_nobleed'), async () => {
        if (!hasRaw) throw { code: 'no_raw_data', detail: '' }
        try { await api.transcribeEnqueue(libraryId, callId, 'resegment', { bleed_filter: false }) }
        catch (e) { throw toError(e).code === 'conflict' ? { code: 'conflict', detail: t('queue.already_open') } : e }
        return true
      }, f => (f.closest('dialog')!.style.width = 'min(40rem,calc(100vw - 2rem))'), t('common.close'))
    if (r) { toast(t('queue.enqueued')); paintJob() }
  }

  // ------------------------------------------------------------ glossário
  /** Prévia (simulação) → confirma → aplica tudo num lote só (Ctrl+Z desfaz o lote inteiro). */
  async function applyGlossary() {
    if (noTranscript()) return // chamada pendente: nada a aplicar
    let rep
    try { rep = await api.glossaryApply(libraryId, callId, null, true) }
    catch (e) { toast(describeGlossaryError(e), 'err'); return }
    if (!rep.blocks_changed) { toast(t('glossary.apply_none')); return }
    const chip = (hit: Hit) => `<span class="rounded-full bg-white/5 px-2 py-0.5 text-[11px] text-zinc-400">${esc(hit.pattern)} → ${esc(hit.replacement)}${hit.count > 1 ? ` ×${hit.count}` : ''}${hit.scope ? ` · ${esc(t(hit.scope === 'client' ? 'glossary.origin_client' : 'glossary.origin_global'))}` : ''}</span>`
    const rows = rep.changes.map(c => {
      const df = diffWords(c.before, c.after)
      return `<li class="rounded-xl border border-white/10 bg-ink-950/60 px-3 py-2">
        <div class="flex flex-wrap items-center gap-1.5"><span class="mr-1 font-mono text-xs text-zinc-500">#${c.seq}</span>${c.rules.map(chip).join('')}</div>
        <p class="mt-1.5 text-xs leading-relaxed text-zinc-500">${df.before}</p>
        <p class="mt-1 text-xs leading-relaxed text-zinc-200">${df.after}</p></li>`
    }).join('')
    const body = `<p class="text-sm text-zinc-300">${esc(t('glossary.apply_summary', { n: rep.blocks_changed }))} · ${esc(t('glossary.apply_replacements', { n: rep.replacements }))}</p>
      <ul class="max-h-[50vh] space-y-2 overflow-y-auto">${rows}</ul>
      <p class="text-xs text-zinc-600">${esc(t('glossary.apply_undo_hint'))}</p>`
    const real = await form(t('glossary.apply_title'), body, t('glossary.apply_ok', { n: rep.blocks_changed }),
      async () => {
        try { return await api.glossaryApply(libraryId, callId, rep.transcript_id, false) }
        catch (e) { throw { code: 'ui', detail: describeGlossaryError(e) } }
      },
      f => (f.closest('dialog')!.style.width = 'min(44rem,calc(100vw - 2rem))'))
    if (!real) return
    await reload()
    toast(t('glossary.applied', { n: real.blocks_changed }))
  }

  /**
   * Modal do glossário da chamada: regras do cliente (adicionar, editar e remover ali mesmo), globais só para leitura
   * (recolhidas) e "Aplicar à chamada". É um <dialog> só: por isso a edição e a confirmação de remoção são inline,
   * e "Classificar"/"Aplicar" fecham este modal antes de abrir o seguinte.
   */
  async function glossaryDialog() {
    const clientId = d.client_id
    let all: Rule[] = []
    const load = async () => { all = await api.glossaryList(clientId != null ? libraryId : null, clientId) }
    try { await load() } catch (e) { toast(describeGlossaryError(e), 'err'); return }
    const own = () => all.filter(r => r.scope === 'client')
    const globals = all.filter(r => r.scope === 'global')

    // `{ id: null }` = regra nova; `{ id }` = editando essa; só um editor por vez
    let editing: { id: number | null } | null = null
    let removing: number | null = null
    let note: { text: string; err?: boolean } | null = null
    let next: 'classify' | null = null
    let f!: HTMLFormElement

    const act = 'rounded-lg px-2 py-1 text-xs text-zinc-400 hover:bg-white/5 hover:text-zinc-100'
    const box = 'rounded-xl border border-white/10 bg-ink-950/60'

    const editorHtml = (r: Rule | null) => `<li data-editor class="space-y-3 rounded-xl border border-violet-400/30 bg-ink-950/60 p-4">
      ${ruleFields(r ?? {}, r?.kind ?? 'replace')}
      <p data-ed-err class="min-h-4 text-sm text-rose-300"></p>
      <div class="flex justify-end gap-2">
        <button type="button" data-ed-cancel class="${btnCls.btn} !px-3 !py-1.5">${esc(t('common.cancel'))}</button>
        <button type="button" data-ed-save class="${btnCls.btnPrimary} !px-3 !py-1.5">${esc(t(r ? 'common.save' : 'common.add'))}</button></div></li>`

    function rowHtml(r: Rule) {
      if (editing?.id === r.id) return editorHtml(r)
      if (removing === r.id) {
        const what = r.kind === 'replace' ? `${r.pattern} → ${r.replacement}` : r.pattern
        return `<li data-rule="${r.id}" class="flex flex-wrap items-center gap-x-3 gap-y-2 rounded-xl border border-rose-400/30 bg-rose-400/[0.05] px-3 py-2">
          <p class="min-w-0 flex-1 text-sm text-rose-100">${esc(t('glossary.remove_confirm', { what }))}</p>
          <div class="flex shrink-0 gap-2">
            <button type="button" data-rm-cancel class="${btnCls.btn} !px-3 !py-1.5">${esc(t('common.cancel'))}</button>
            <button type="button" data-rm-ok class="rounded-xl border border-rose-400/50 bg-rose-500/15 px-3 py-1.5 text-sm font-medium text-rose-100 hover:bg-rose-500/25">${esc(t('common.remove'))}</button></div></li>`
      }
      return `<li data-rule="${r.id}" class="flex items-center gap-3 ${box} px-3 py-2">
        <p class="min-w-0 flex-1 break-words font-mono text-sm">${ruleText(r)}</p>${caseBadge(r)}
        <div class="flex shrink-0 items-center">
          <button type="button" data-edit class="${act}">${esc(t('common.edit'))}</button>
          <button type="button" data-remove class="rounded-lg px-2 py-1 text-xs text-zinc-400 hover:bg-rose-400/10 hover:text-rose-200">${esc(t('common.remove'))}</button></div></li>`
    }

    /** Regras do cliente em dois grupos (substituições, termos); o editor de regra nova abre no topo. */
    function clientHtml() {
      if (clientId == null) {
        return `<div class="rounded-xl border border-amber-400/20 bg-amber-400/[0.04] p-4">
          <h3 class="text-sm font-semibold text-amber-100">${esc(t('glossary.no_client_title'))}</h3>
          <p class="mt-1 text-sm text-zinc-400">${esc(t('glossary.no_client_body'))}</p>
          <button type="button" data-classify class="${btnCls.btnPrimary} mt-3 !px-3 !py-1.5">${esc(t('call.assign'))}</button></div>`
      }
      const rs = own()
      const group = (kind: RuleKind, label: string) => {
        const list = rs.filter(r => r.kind === kind)
        return list.length ? `<li role="presentation" class="pt-1 text-[11px] font-semibold uppercase tracking-wider text-zinc-600 first:pt-0">${esc(label)}</li>${list.map(rowHtml).join('')}` : ''
      }
      const rows = group('replace', t('glossary.tab_replace')) + group('term', t('glossary.tab_term'))
      return `<div class="flex items-center justify-between gap-3">
          <h3 class="text-sm font-semibold text-zinc-200">${esc(t('glossary.client_rules'))} <span class="text-xs font-normal tabular-nums text-zinc-500">${fmtNumber(rs.length)}</span></h3>
          <button type="button" data-add class="${btnCls.btn} !px-3 !py-1.5">+ ${esc(t('glossary.modal_add'))}</button></div>
        <p class="mt-0.5 text-xs text-zinc-500">${esc(t('glossary.scope_client', { name: d.client_name ?? '' }))}</p>
        <ul class="mt-3 space-y-2 ${editing ? '' : 'max-h-[38vh] overflow-y-auto pr-1'}">${editing?.id === null ? editorHtml(null) : ''}${rows || (editing?.id === null ? '' : `<li class="rounded-xl border border-dashed border-white/10 px-4 py-6 text-center text-sm text-zinc-600">${esc(t('glossary.client_empty'))}</li>`)}</ul>`
    }

    const globalHtml = () => {
      const rows = globals.map(r => `<li class="flex items-center gap-3 ${r.overridden ? 'opacity-50' : ''}">
        <p class="min-w-0 flex-1 break-words font-mono text-sm">${ruleText(r, r.overridden ? 'line-through decoration-zinc-500' : '')}</p>${caseBadge(r)}</li>`).join('')
      return `<details class="${box}">
        <summary class="cursor-pointer select-none px-4 py-2.5 text-sm font-semibold text-zinc-300 hover:text-white">${esc(t('glossary.global_rules'))} <span class="text-xs font-normal tabular-nums text-zinc-500">${fmtNumber(globals.length)}</span></summary>
        <div class="border-t border-white/5 px-4 py-3">
          <p class="text-xs text-zinc-500">${esc(t('glossary.global_hint'))}</p>
          <ul class="mt-2 max-h-[28vh] space-y-1.5 overflow-y-auto pr-1">${rows || `<li class="text-sm text-zinc-600">${esc(t('glossary.global_empty'))}</li>`}</ul>
          <a href="#/glossary?global" data-to-glossary class="mt-3 inline-block text-xs text-violet-300 hover:underline">${esc(t('glossary.manage_global'))} →</a></div></details>`
    }

    function paint() {
      f.querySelector('[data-client]')!.innerHTML = clientHtml()
      const n = f.querySelector<HTMLElement>('[data-note]')!
      n.textContent = note?.text ?? ''
      n.className = `min-h-4 text-sm ${note?.err ? 'text-rose-300' : 'text-emerald-300'}`
      const ed = f.querySelector<HTMLElement>('[data-editor]')
      if (ed) { syncKind(ed); ed.querySelector<HTMLInputElement>('[name="pattern"]')!.focus() }
    }
    const reload = async () => { await load(); paint() }

    async function saveRule() {
      const ed = f.querySelector<HTMLElement>('[data-editor]')!
      const err = ed.querySelector<HTMLElement>('[data-ed-err]')!
      err.textContent = ''
      const cur = editing!.id == null ? null : own().find(r => r.id === editing!.id)!
      try {
        const input = readRule(ed)
        try {
          if (cur) await api.glossaryUpdate({ ...input, scope: 'client', libraryId: cur.library_id, id: cur.id })
          else await api.glossaryAdd({ ...input, scope: 'client', libraryId, clientId })
        } catch (e) { throw { code: 'ui', detail: describeGlossaryError(e) } }
      } catch (e) { err.textContent = describeError(e); return }
      editing = null
      note = { text: t(cur ? 'call.saved' : 'glossary.added') }
      try { await reload() } catch (e) { note = { text: describeGlossaryError(e), err: true }; paint() }
    }

    async function removeRule() {
      const r = own().find(x => x.id === removing)
      removing = null
      if (!r) { paint(); return }
      try { await api.glossaryRemove('client', r.library_id, r.id); note = { text: t('glossary.removed') }; await reload() }
      catch (e) { note = { text: describeGlossaryError(e), err: true }; paint() }
    }

    const body = `<p class="text-sm text-zinc-500">${esc(t('glossary.modal_intro'))}</p>
      <section data-client></section>
      ${globalHtml()}
      <p data-note aria-live="polite"></p>
      ${noTranscript() ? `<p class="rounded-xl border border-white/10 bg-ink-950/50 px-3 py-2 text-xs text-zinc-500">${esc(t('glossary.pending_note'))}</p>` : ''}`

    const r = await form(t('glossary.title'), body, t('glossary.apply_call'),
      async () => {
        if (f.querySelector('[data-editor]')) throw { code: 'ui', detail: t('glossary.finish_first') }
        return 'apply' as const
      },
      fm => {
        f = fm
        f.noValidate = true // o campo do editor aberto não pode barrar o "Aplicar à chamada"
        f.closest('dialog')!.style.width = 'min(40rem,calc(100vw - 2rem))'
        // sem transcrição não há o que aplicar; as regras (termos) valem para quando for transcrita
        if (noTranscript()) f.querySelector<HTMLElement>('[type="submit"]')!.hidden = true
        paint()
        // Enter dentro do editor salva a regra (não dispara o "Aplicar à chamada" do formulário)
        f.addEventListener('keydown', e => {
          const el = e.target as HTMLElement
          if (e.key === 'Enter' && el.closest('[data-editor]') && !el.matches('button')) { e.preventDefault(); void saveRule() }
        })
        f.addEventListener('click', e => {
          const el = e.target as HTMLElement
          const id = () => Number(el.closest<HTMLElement>('[data-rule]')!.dataset.rule)
          if (el.closest('[data-add]')) { editing = { id: null }; removing = null; note = null; paint() }
          else if (el.closest('[data-edit]')) { editing = { id: id() }; removing = null; note = null; paint() }
          else if (el.closest('[data-remove]')) { removing = id(); editing = null; note = null; paint() }
          else if (el.closest('[data-ed-cancel]')) { editing = null; paint() }
          else if (el.closest('[data-ed-save]')) void saveRule()
          else if (el.closest('[data-rm-cancel]')) { removing = null; paint() }
          else if (el.closest('[data-rm-ok]')) void removeRule()
          else if (el.closest('[data-classify]')) { next = 'classify'; f.closest('dialog')!.close() }
          else if (el.closest('[data-to-glossary]')) f.closest('dialog')!.close() // a navegação segue pelo link
        })
      }, t('common.close'))
    if (next === 'classify') await assign(true)
    else if (r === 'apply') await applyGlossary()
  }

  // ------------------------------------------------------------ player de áudio (issue #22)
  const noAudioReason = (): 'deleted' | 'none' | null => (d.audio.deleted_at ? 'deleted' : !d.audio.mic_path && !d.audio.sys_path ? 'none' : null)
  const noteFor = (why: string, error = '') => (why === 'none' ? t('call.audio_none') : why === 'missing' ? t('player.missing') : t('player.unreadable', { error }))
  /** Os trechos em ordem de início (busca binária por posição); refeito a cada `draw()`, que recria o DOM. */
  let starts: { t: number; id: number }[] = []
  let playingId: number | null = null
  const indexStarts = () => { starts = d.blocks.map(b => ({ t: b.t_start, id: b.id })).sort((a, b) => a.t - b.t) }

  /** O trecho que está tocando em `s`: o último que já começou (folga de 50 ms para o arredondamento do tempo). */
  function blockAt(s: number): number | null {
    let lo = 0, hi = starts.length
    while (lo < hi) { const m = (lo + hi) >> 1; if (starts[m].t <= s + 0.05) lo = m + 1; else hi = m }
    return lo ? starts[lo - 1].id : null
  }

  /** Destaca o trecho tocando (só troca um atributo, sem refazer a página); com `follow`, rola até ele se saiu da tela. */
  function paintPlaying(scroll = true) {
    const id = player ? blockAt(player.position()) : null
    if (!scroll) playingId = null
    if (id === playingId) return
    playingId = id
    el.querySelectorAll('[data-playing]').forEach(b => b.removeAttribute('data-playing'))
    const b = id == null ? null : el.querySelector<HTMLElement>(`#b-${id}`)
    b?.setAttribute('data-playing', '')
    if (scroll && b && player?.follow() && !b.hidden) followBlock(b)
  }

  function followBlock(b: HTMLElement) {
    const box = el.getBoundingClientRect(), r = b.getBoundingClientRect()
    const top = box.top + (parseFloat(el.style.getPropertyValue('--hdr')) || 0) + 48
    const bottom = box.bottom - (el.querySelector('#dock')?.getBoundingClientRect().height ?? 0) - 8
    if (r.top < top || r.bottom > bottom) b.scrollIntoView({ block: 'center', behavior: 'smooth' })
  }

  /** Abre o player no Rust e mostra a barra; sem áudio mostra só a linha que explica. */
  async function startPlayer() {
    if (player) return
    const why = noAudioReason()
    if (why === 'deleted') return // a linha de "áudio apagado" sai de `d` (audioNoteHtml)
    if (why) { audioNote = noteFor(why); paintNote(); return }
    try {
      const info = await api.playerOpen(libraryId, callId)
      if (!alive) { void api.playerClose(libraryId, callId); return } // saiu da tela enquanto abria
      if (!info.available) { audioNote = noteFor(info.reason ?? 'none'); paintNote(); return }
      player = createPlayer({
        libraryId, callId, duration: info.duration_s, onPosition: () => paintPlaying(), cuts: d.cuts,
        onCutToggle: () => void setCutMode(!cutMode), onPending: () => paintCutPanel(),
      })
      el.querySelector('#dock')?.append(player.el)
    } catch (e) {
      if (alive) { audioNote = noteFor('unreadable', describeError(e)); paintNote() }
    }
  }

  /** A única linha sob o cabeçalho sobre o áudio: apagado (com a data), sem trilhas, arquivos ausentes ou ilegível. */
  function audioNoteHtml() {
    const text = d.audio.deleted_at ? t('audio.gone_note', { date: fmtDate(d.audio.deleted_at) }) : audioNote
    return text ? `<p id="audio-note" class="mt-2 rounded-xl border border-white/10 bg-white/[0.03] px-3 py-2 text-xs text-zinc-400">${esc(text)}</p>` : ''
  }

  /** A linha sem refazer a página (as chamadas pendentes já dizem isso no próprio estado). */
  function paintNote() {
    el.querySelector('#audio-note')?.remove()
    if (noTranscript()) return
    el.querySelector('#job-strip')?.insertAdjacentHTML('afterend', audioNoteHtml())
  }

  /** O áudio sumiu (apagado por outra tela): fecha o player, que ainda tem os arquivos abertos. O desenho explica. */
  function syncAudio() {
    if (!noAudioReason() || !player) return
    void player.dispose()
    player = null
    cutMode = false
  }

  // ------------------------------------------------------------ cortes de áudio (#23)
  /** Depois de qualquer mudança nos cortes (salvar, remover, excluir/restaurar trecho, desfazer): a onda e o motor acompanham. */
  function syncCuts() {
    const now = cutSig()
    if (now === cutsSeen) return
    cutsSeen = now
    if (!player) return
    player.setCuts(d.cuts)
    void api.playerSetCuts(libraryId, callId).catch(() => {})
  }

  const spanText = (a: number, b: number) => `${fmtTime(a, d.duration_s >= 3600)} – ${fmtTime(b, d.duration_s >= 3600)}`
  const spanLen = (a: number, b: number) => { const s = b - a; return s >= 60 ? `${Math.floor(s / 60)} min ${String(Math.round(s % 60)).padStart(2, '0')} s` : `${s >= 10 ? Math.round(s) : s.toFixed(1)} s` }

  /** O quadro de cortes: cortes novos (ainda não salvos) e os salvos, cada um com seu remover. */
  function cutPanelHtml() {
    const fresh = player?.pending() ?? []
    const row = (a: number, b: number, tag: string, btn: string) => `<li class="flex items-center gap-3 rounded-xl border border-white/10 bg-ink-950/60 px-3 py-1.5 text-xs">
        <span class="font-mono tabular-nums text-zinc-200">${esc(spanText(a, b))}</span>
        <span class="font-mono tabular-nums text-zinc-500">${esc(spanLen(a, b))}</span>${tag}<span class="ml-auto">${btn}</span></li>`
    const rm = (attrs: string, label: string) => `<button type="button" ${attrs} class="rounded-lg px-2 py-1 text-zinc-400 hover:bg-white/5 hover:text-rose-300">${esc(label)}</button>`
    const pill = (text: string, cls: string) => `<span class="rounded-full px-2 py-px text-[10px] font-medium ${cls}">${esc(text)}</span>`
    const items = [
      ...fresh.map(([a, b], i) => row(a, b, pill(t('cut.new'), 'bg-violet-400/15 text-violet-200'), rm(`data-cut="drop" data-i="${i}"`, t('cut.discard')))),
      ...d.cuts.map(c => row(c.t_start, c.t_end,
        c.block_id != null ? pill(c.block_seq != null ? t('cut.from_block', { seq: c.block_seq }) : t('cut.from_block_gone'), 'bg-white/5 text-zinc-400') : '',
        // o corte de um trecho excluído sai restaurando o trecho; só o manual tem "remover" aqui
        c.block_id != null && c.block_seq != null ? '' : rm(`data-cut="remove" data-id="${c.id}"`, t('cut.remove')))),
    ]
    return `<div id="cut-panel" ${cutMode ? '' : 'hidden'} class="pointer-events-auto mx-auto mb-3 max-w-6xl rounded-2xl border border-violet-400/30 bg-ink-900/95 p-4 shadow-2xl backdrop-blur-md">
      <div class="flex flex-wrap items-start justify-between gap-x-6 gap-y-3">
        <div class="min-w-[16rem] flex-1">
          <h2 class="text-sm font-semibold text-white">${esc(t('cut.title'))}</h2>
          <p class="mt-1 text-xs text-zinc-400">${esc(t('cut.hint'))}</p>
          <p data-cut-note class="mt-1.5 rounded-lg bg-white/[0.03] px-2.5 py-1.5 text-xs text-zinc-500">${esc(t('cut.silence_note'))}</p>
        </div>
        <div class="flex shrink-0 items-center gap-2">
          <span class="text-xs text-zinc-500">${fresh.length ? esc(t('cut.pending_count', { n: fresh.length })) : ''}</span>
          <button type="button" data-cut="cancel" class="${btnCls.btn}">${esc(t('cut.cancel'))}</button>
          <button type="button" data-cut="save" ${fresh.length ? '' : 'disabled'} class="${btnCls.btnPrimary} disabled:cursor-not-allowed disabled:opacity-40">${esc(t('cut.save'))}</button>
        </div>
      </div>
      <ul data-cut-list class="mt-3 max-h-40 space-y-1 overflow-y-auto">${items.join('') || `<li class="px-1 py-1 text-xs text-zinc-600">${esc(t('cut.empty'))}</li>`}</ul></div>`
  }

  /** Refaz só o quadro (um corte novo marcado na onda), sem redesenhar a página. */
  function paintCutPanel() {
    const box = el.querySelector('#cut-panel')
    if (!box) return
    const tmp = document.createElement('div')
    tmp.innerHTML = cutPanelHtml()
    box.replaceWith(tmp.firstElementChild!)
  }

  async function setCutMode(on: boolean) {
    if (on === cutMode || !player) return
    if (on && !(await leave())) return
    if (on) await setSelecting(false)
    cutMode = on
    player.setCutMode(on)
    paintCutPanel()
    el.querySelector<HTMLElement>('#cut-panel')?.scrollIntoView({ block: 'nearest' })
  }

  /** Mostra quantos trechos sairiam, confirma e grava tudo num lote só (desfazer = um passo). */
  async function saveCuts() {
    const spans = player?.pending() ?? []
    if (!player || !spans.length) return
    try {
      const preview = await api.previewCuts(libraryId, callId, spans)
      if (!preview.added.length) { toast(t('cut.nothing_new')); player.setPending([]); paintCutPanel(); return }
      const n = preview.deleted_blocks.length
      if (!(await confirmDialog(t('cut.confirm_title', { n: preview.added.length }), n ? t('cut.confirm_body', { n }) : t('cut.confirm_none'), t('cut.confirm_action')))) return
      const r = await api.addCuts(libraryId, callId, spans)
      await setCutMode(false)
      await reload()
      const gone = r.deleted_blocks.length
      toastAction(gone ? `${t('cut.done', { n: r.added.length })} · ${t('delete.done', { n: gone })}` : t('cut.done', { n: r.added.length }), t('delete.undo'), () => void undo())
    } catch (e) { toast(describeError(e), 'err') }
  }

  async function removeCut(id: number) {
    try {
      const r = await api.removeCut(libraryId, callId, id)
      await reload()
      const back = r.restored_blocks.length
      toast(back ? `${t('cut.removed')} · ${t('delete.restored', { n: back })}` : t('cut.removed'))
    } catch (e) { toast(describeError(e), 'err') }
  }

  function bind() {
    $('#q').addEventListener('input', runSearch)
    $('#title-edit').addEventListener('click', editTitle)
    $('#history').addEventListener('click', showHistory)
    $('#assign').addEventListener('click', () => void assign())
    $('#glossary-btn').addEventListener('click', glossaryDialog)
    el.querySelector('#select-btn')?.addEventListener('click', () => void setSelecting(!selecting))
    el.querySelector('#speakers-btn')?.addEventListener('click', speakersDialog)
    el.querySelector('#redo-btn')?.addEventListener('click', redoDialog)
    el.querySelector('#audio-delete-btn')?.addEventListener('click', deleteAudioDialog)
    el.querySelector('#bleed-btn')?.addEventListener('click', bleedDialog)
    el.querySelector<HTMLSelectElement>('#version')?.addEventListener('change', async e => {
      await api.setActiveTranscript(libraryId, callId, Number((e.target as HTMLSelectElement).value))
      await reload(false)
    })
    io?.disconnect()
    io = new IntersectionObserver(es => es.forEach(e => {
      if (!e.isIntersecting) return
      el.querySelectorAll<HTMLElement>('[data-nav]').forEach(a => {
        const on = a.dataset.nav === e.target.id
        a.classList.toggle('border-violet-400', on)
        a.classList.toggle('text-zinc-100', on)
        a.classList.toggle('border-transparent', !on)
      })
    }), { root: el, rootMargin: '-30% 0px -60% 0px' })
    el.querySelectorAll('[data-section]').forEach(s => io!.observe(s))
    el.querySelectorAll<HTMLAnchorElement>('[data-nav]').forEach(a => a.addEventListener('click', e => {
      e.preventDefault()
      el.querySelector(a.getAttribute('href')!)?.scrollIntoView({ behavior: 'smooth' })
    }))
  }

  // eventos delegados (sobrevivem a `draw`)
  const onInput = (e: Event) => {
    const ta = e.target as HTMLElement
    const b = ta.closest?.('[data-editing]') as HTMLElement | null
    if (!b || !ta.matches('textarea')) return
    growEditor(ta as HTMLTextAreaElement)
    syncEditor(b)
    setStatus('')
  }
  const onKey = (e: KeyboardEvent) => {
    const target = e.target as HTMLElement
    const eb = target.closest?.('[data-editing]') as HTMLElement | null
    if (eb && target.matches('textarea')) {
      if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) { e.preventDefault(); void saveEditor(eb) }
      if (e.key === 'Escape') { e.preventDefault(); cancelEditor(eb) }
      return // não roubar teclas durante a edição
    }
    if (document.querySelector('dialog[open]')) return
    const q = $<HTMLInputElement>('#q')
    if (target === q && e.key === 'Escape') { e.preventDefault(); q.value = ''; runSearch(); q.blur(); return }
    if (selecting && e.key === 'Escape') { e.preventDefault(); void setSelecting(false); return }
    // espaço/Enter numa mensagem em foco marca/desmarca (a mensagem faz o papel de caixa de seleção)
    const sb = selecting ? target.closest<HTMLElement>('[data-block]') : null
    if (sb && target === sb && (e.key === ' ' || e.key === 'Enter')) { e.preventDefault(); toggleSelect(sb, e.shiftKey); return }
    if (target.matches('input, textarea, select')) return
    // espaço toca/pausa, menos em campo de texto, no editor de trecho ou sobre um controle que já usa o espaço
    // (links e a onda ficam de fora da lista: clicar no tempo de um trecho ou na onda deixa o foco neles)
    if (e.key === ' ' && player && !editor && !e.ctrlKey && !e.metaKey && !e.altKey && !target.closest('button, summary, [contenteditable], [role="checkbox"]')) {
      e.preventDefault()
      if (!e.repeat) player.toggle()
      return
    }
    if (e.key === '/') { e.preventDefault(); q.focus() }
    if (e.key === 'Escape') { q.value = ''; runSearch(); q.blur() }
    if ((e.ctrlKey || e.metaKey) && !e.shiftKey && e.key.toLowerCase() === 'z') { e.preventDefault(); undo() }
  }
  const onClick = (e: MouseEvent) => {
    const target = e.target as HTMLElement
    if (onWhyClick(target)) return
    const act = target.closest<HTMLElement>('[data-tx]')?.dataset.tx
    if (act) {
      const job = jobForCall(libraryId, callId)
      if (act === 'enqueue') void enqueueCall(libraryId, callId)
      else if (act === 'dismiss' && job) { dismissed.add(job.id); paintJob() }
      else if (act === 'cancel' && job) void cancelJob(job)
      else if (act === 'retry') void (job && job.state === 'failed' ? retryJob(job) : enqueueCall(libraryId, callId))
      return
    }
    const cut = target.closest<HTMLElement>('[data-cut]')
    if (cut) {
      const what = cut.dataset.cut
      if (what === 'cancel') void setCutMode(false)
      else if (what === 'save') void saveCuts()
      else if (what === 'drop') { const v = player?.pending() ?? []; v.splice(Number(cut.dataset.i), 1); player?.setPending(v); paintCutPanel() }
      else if (what === 'remove') void removeCut(Number(cut.dataset.id))
      return
    }
    const sel = target.closest<HTMLElement>('[data-sel]')?.dataset.sel
    if (sel) {
      if (sel === 'all') selectAll(true)
      else if (sel === 'none') selectAll(false)
      else if (sel === 'cancel') void setSelecting(false)
      else if (sel === 'delete') void deleteBlocks([...selected])
      return
    }
    const blk = target.closest<HTMLElement>('[data-block]')
    if (blk && selecting) { e.preventDefault(); toggleSelect(blk, e.shiftKey); return }
    if (blk && target.closest('[data-delete]')) { void deleteBlocks([idOf(blk)]); return }
    if (blk && target.closest('[data-edit]')) { openEditor(blk); return }
    if (blk && target.closest('[data-save]')) { void saveEditor(blk); return }
    if (blk && target.closest('[data-cancel]')) { cancelEditor(blk); return }
    const rev = target.closest('[data-revert]')
    if (rev) { const b = rev.closest<HTMLElement>('[data-block]')!; void save(b, () => api.revertBlock(libraryId, blockData(b).id)).then(r => { if (r) closeEditor(b) }); return }
    const sb = target.closest('[data-speaker-btn]')
    if (sb && sb.closest('[data-editing]')) { void speakerMenu(sb.closest<HTMLElement>('[data-block]')!); return }
    const anchor = target.closest<HTMLAnchorElement>('[data-anchor]')
    if (anchor) {
      e.preventDefault()
      history.replaceState(null, '', anchor.getAttribute('href'))
      const ab = anchor.closest<HTMLElement>('[data-block]')!
      // com player: o tempo do trecho pula o áudio para ali e toca; sem áudio, só leva ao trecho como antes
      if (player) player.seek(blockData(ab).t_start, true)
      else flash(Number(ab.dataset.block))
    }
  }
  const flash = (id: number) => {
    const b = el.querySelector<HTMLElement>(`#b-${id}`)
    if (!b) return
    b.scrollIntoView({ block: 'center' })
    const bubble = b.firstElementChild as HTMLElement
    bubble.removeAttribute('data-flash'); void bubble.offsetWidth; bubble.setAttribute('data-flash', '')
  }
  el.addEventListener('input', onInput)
  el.addEventListener('click', onClick)
  document.addEventListener('keydown', onKey)
  // recarregar/fechar a página: melhor esforço (sair da tela e fechar a janela passam por `leave`)
  const flushOnUnload = () => {
    const b = openBlock()
    if (b && isDirty(b) && norm(editorOf(b)!.value)) void api.setBlockText(libraryId, blockData(b).id, norm(editorOf(b)!.value))
  }
  window.addEventListener('beforeunload', flushOnUnload)

  // fila: a faixa/estado da chamada acompanha; mudança de estado da tarefa desta chamada recarrega (versão nova, falha...)
  const jobSig = () => { const j = jobForCall(libraryId, callId); return j ? `${j.id}:${j.state}` : '' }
  let sig = jobSig()
  const busy = () => !!el.querySelector('[data-editing]') || !!document.querySelector('dialog[open]')
  const offs = [
    subscribeTx('queue', () => { const n = jobSig(); if (n !== sig) { sig = n; if (!busy()) void reload(); else paintJob() } else paintJob() }),
    subscribeTx('progress', paintJob),
  ]

  draw()
  void startPlayer()
  if (params.get('glossary')) { history.replaceState(null, '', location.hash.split('?')[0]); void glossaryDialog() } // volta do "Classificar" do modal
  const target = Number(params.get('b'))
  const q = params.get('q')
  if (q) { $<HTMLInputElement>('#q').value = q; runSearch() }
  if (target) requestAnimationFrame(() => flash(target))

  return {
    refresh: () => reload(),
    busy,
    leave,
    dispose: () => {
      alive = false
      player?.dispose()
      player = null
      offs.forEach(f => f())
      io?.disconnect()
      ro?.disconnect()
      el.style.removeProperty('--hdr')
      el.removeEventListener('input', onInput)
      el.removeEventListener('click', onClick)
      document.removeEventListener('keydown', onKey)
      window.removeEventListener('beforeunload', flushOnUnload)
    },
  }
}

