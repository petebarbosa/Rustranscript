import { api, type BlockEdit, type BlockInfo, type BlockSuggestion, type CallDetail, type Hit, type HistoryEntry, type Scope, type SpeakerInfo } from '../api'
import { t } from '../i18n'
import { hooks, libName, meName, store, type View } from '../store'
import { assignDialog, btnCls, describeError, describeGlossaryError, form, inputCls, renameDialog } from '../dialogs'
import { callTitle, diffWords, esc, fmtClock, fmtDate, fmtDuration, fmtNumber, fmtTime, fold, h, rx, toast } from '../util'

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
  for (const b of d.blocks) {
    let key: number, label: string, sub = ''
    if (chs.length) {
      const owner = chs.filter(c => c.t <= b.t_start).pop()
      key = owner ? owner.t : -1
      label = owner ? owner.title : t('call.start')
      sub = fmtTime(Math.max(key, 0))
    } else {
      key = Math.floor(b.t_start / BUCKET) * BUCKET
      label = `${fmtTime(key)} – ${fmtTime(key + BUCKET)}`
    }
    const last = out[out.length - 1]
    if (!last || last.id !== `s-${key}`) out.push({ id: `s-${key}`, label, sub, blocks: [] })
    out[out.length - 1].blocks.push(b)
  }
  return out
}

export async function renderCall(el: HTMLElement, libraryId: number, callId: number, params: URLSearchParams): Promise<View> {
  let d = await api.callDetail(libraryId, callId)
  let editing = (() => { try { return localStorage.getItem('edit-mode') === '1' } catch { return false } })()
  const timers = new Map<number, ReturnType<typeof setTimeout>>()
  let io: IntersectionObserver | null = null
  let ro: ResizeObserver | null = null

  const speakerById = () => new Map(d.speakers.map(s => [s.id, s]))
  const styleFor = (s: SpeakerInfo | undefined, order: Map<number, number>): string[] => {
    if (!s || s.track === 'mic') return ME_STYLE
    if (!order.has(s.id)) order.set(s.id, order.size)
    return PALETTE[order.get(s.id)! % PALETTE.length]
  }
  const nameOf = (s: SpeakerInfo | undefined) => (s?.name ?? (s?.track === 'mic' ? meName() || t('speaker.me') : s?.label ?? '?'))

  function blockHtml(b: BlockInfo, spk: Map<number, SpeakerInfo>, order: Map<number, number>) {
    const s = spk.get(b.speaker_id)
    const [label, box] = styleFor(s, order)
    const me = s?.track === 'mic'
    const edited = b.edited ? ` data-edited title="${esc(t('call.original', { text: b.original_text }))}"` : ''
    return `<article id="b-${b.id}" data-block="${b.id}"${edited} class="flex ${me ? 'justify-end' : 'justify-start'}">
      <div class="min-w-[14rem] max-w-[46rem] rounded-2xl border ${box} px-4 py-3 shadow-sm">
        <div class="mb-1 flex items-center gap-2 text-xs">
          <button type="button" data-speaker-btn class="font-semibold ${label}">${esc(nameOf(s))}</button>
          <a href="#/call/${libraryId}/${callId}?b=${b.id}" data-anchor class="font-mono text-zinc-500 hover:text-zinc-200">${fmtTime(b.t_start)}</a>
          <span data-badge class="hidden items-center rounded-full bg-amber-400/10 px-2 py-px text-[10px] font-medium text-amber-300">${esc(t('call.edited'))}</span>
          <button type="button" data-revert class="hidden items-center rounded-full px-2 py-px text-[10px] text-zinc-500 hover:bg-white/5 hover:text-zinc-200">↺ ${esc(t('call.revert'))}</button>
        </div>
        <p data-text class="leading-relaxed text-zinc-200">${esc(b.text)}</p>
      </div></article>`
  }

  /** Chamada recém-gravada: sem transcrição (transcript_id null; blocos, falantes e capítulos vêm vazios). */
  const noTranscript = () => d.transcript_id == null

  /** Estado vazio da chamada pendente: o áudio existe, a transcrição chega depois (fase 4). */
  function pendingHtml() {
    const st = d.transcription_state
    const tracks = [
      d.audio.mic_path && t('record.track_mic'), d.audio.sys_path && t('record.track_sys'),
    ].filter(Boolean) as string[]
    const tone = st === 'failed' ? 'border-rose-400/30 bg-rose-400/[0.05]' : 'border-white/10 bg-ink-900/60'
    return `<div id="pending-state" class="mt-4 rounded-2xl border ${tone} p-8 text-center">
      <p class="text-4xl text-zinc-500">${st === 'running' ? '◔' : st === 'failed' ? '⚠' : '◌'}</p>
      <h2 class="mt-3 text-lg font-semibold text-white">${esc(t(`transcription.${st}_title`))}</h2>
      <p class="mx-auto mt-2 max-w-xl text-sm text-zinc-400">${esc(t(`transcription.${st}_body`, { error: d.transcription_error ?? '' }))}</p>
      <p class="mt-5 text-xs text-zinc-500">${d.audio.deleted_at
        ? esc(t('call.audio_deleted'))
        : tracks.length ? `${esc(t('call.audio_present'))}: ${tracks.map(x => `<span class="ml-1 rounded-full bg-white/5 px-2 py-0.5 text-zinc-300">${esc(x)}</span>`).join('')}` : esc(t('call.audio_none'))}</p>
    </div>`
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
    const versions = d.transcripts.length > 1
      ? `<select id="version" class="rounded-full border border-white/10 bg-ink-900 px-2.5 py-0.5 text-xs text-zinc-300">${d.transcripts
          .map(v => `<option value="${v.id}" ${v.id === d.transcript_id ? 'selected' : ''}>${esc(t('call.version', { v: v.version }))}${v.model ? ' · ' + esc(v.model) : ''}</option>`).join('')}</select>`
      : ''
    const nav = secs.map(s => `<a href="#${s.id}" data-nav="${s.id}" class="block rounded-lg border-l-2 border-transparent px-3 py-1.5 text-sm text-zinc-400 hover:bg-white/5 hover:text-zinc-100">${esc(s.label)}${s.sub ? `<span class="block font-mono text-[11px] text-zinc-600">${esc(s.sub)}</span>` : ''}</a>`).join('')
    const body = secs.map(s => `<section id="${s.id}" data-section class="scroll-mt-[calc(var(--hdr)+3rem)]">
        <h2 class="sticky top-[var(--hdr)] z-10 -mx-2 mb-4 mt-10 flex items-center gap-3 bg-ink-950/90 px-2 py-1.5 text-xs font-semibold uppercase tracking-wider text-zinc-500 backdrop-blur-md first:mt-0"><span>${esc(s.label)}</span><span class="h-px flex-1 bg-white/10"></span></h2>
        <div class="space-y-3">${s.blocks.map(b => blockHtml(b, spk, order)).join('')}</div></section>`).join('')

    el.innerHTML = `
    <header class="sticky top-0 z-30 border-b border-white/10 bg-ink-950/85 backdrop-blur-xl">
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
            ? `<span class="rounded-full border px-2.5 py-0.5 text-xs ${d.transcription_state === 'failed' ? 'border-rose-400/30 bg-rose-400/10 text-rose-300' : d.transcription_state === 'running' ? 'border-sky-400/30 bg-sky-400/10 text-sky-300' : 'border-amber-400/30 bg-amber-400/10 text-amber-300'}">${esc(t(`transcription.${d.transcription_state === 'done' ? 'pending' : d.transcription_state}`))}</span>`
            : chip(t('list.words', { n: d.words, count: fmtNumber(d.words) }))}
          ${versions}
        </div>
        <div class="mt-3 flex items-center gap-2">
          <div class="relative flex-1" ${pending ? 'hidden' : ''}>
            <input id="q" type="search" autocomplete="off" placeholder="${esc(t('call.search'))}"
              class="w-full rounded-xl border border-white/10 bg-ink-900 py-2 pl-4 pr-28 text-sm text-zinc-100 placeholder:text-zinc-600 focus:border-violet-400/60 focus:outline-none focus:ring-2 focus:ring-violet-400/20">
            <span id="count" class="pointer-events-none absolute right-3 top-2 text-xs text-zinc-500"></span>
          </div>
          <button id="assign" type="button" class="shrink-0 rounded-xl border border-white/10 bg-ink-900 px-3 py-2 text-sm text-zinc-300 hover:border-violet-400/50">${esc(t('call.assign'))}</button>
          <button id="apply-glossary" type="button" ${pending ? 'hidden' : ''} title="${esc(t('glossary.apply_hint'))}" class="shrink-0 rounded-xl border border-white/10 bg-ink-900 px-3 py-2 text-sm text-zinc-300 hover:border-violet-400/50">${esc(t('glossary.apply'))}</button>
          <button id="history" type="button" class="shrink-0 rounded-xl border border-white/10 bg-ink-900 px-3 py-2 text-sm text-zinc-300 hover:border-violet-400/50">${esc(t('call.history'))}</button>
          <button id="edit-toggle" type="button" ${pending ? 'hidden' : ''} aria-pressed="false" class="shrink-0 rounded-xl border border-white/10 bg-ink-900 px-4 py-2 text-sm text-zinc-300 hover:border-violet-400/50">✎ ${esc(t('call.edit'))}</button>
        </div>
        <div class="mt-1.5 flex min-h-4 items-center justify-between gap-3 text-xs">
          <span id="edit-hint" hidden class="text-zinc-500">${esc(t('call.edit_hint'))}</span>
          <span id="save-status" class="ml-auto"></span>
        </div>
      </div>
    </header>
    <div class="mx-auto grid max-w-6xl gap-8 px-6 py-8 ${pending ? '' : 'lg:grid-cols-[14rem_1fr]'}">
      <aside ${pending ? 'hidden' : ''} class="hidden lg:sticky lg:top-[calc(var(--hdr)+1rem)] lg:block lg:self-start">
        <nav class="max-h-[calc(100vh-var(--hdr)-2rem)] space-y-0.5 overflow-y-auto pr-2">
          <p class="px-3 pb-2 text-[11px] font-semibold uppercase tracking-wider text-zinc-600">${esc(t('call.nav'))}</p>${nav}</nav>
      </aside>
      <main id="blocks" class="min-w-0">${pending ? pendingHtml() : `${body}
        <p class="mt-14 text-center text-xs text-zinc-700">${esc(t('call.end'))}</p>`}</main>
    </div>`
    ro?.disconnect()
    const header = el.querySelector('header')!
    const syncHeader = () => el.style.setProperty('--hdr', `${header.offsetHeight}px`)
    syncHeader()
    ro = new ResizeObserver(syncHeader)
    ro.observe(header)
    bind()
    setEditing(editing)
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
      if (document.activeElement === p) continue
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
  }

  function setEditing(on: boolean) {
    editing = on
    $('#blocks').classList.toggle('editing', on)
    $('#edit-toggle').setAttribute('aria-pressed', String(on))
    $('#edit-hint').hidden = !on
    for (const b of blocks()) {
      const p = txt(b)
      if (on) {
        p.contentEditable = 'plaintext-only'
        if (p.contentEditable !== 'plaintext-only') p.contentEditable = 'true'
        p.spellcheck = false
      } else p.removeAttribute('contenteditable')
    }
    try { localStorage.setItem('edit-mode', on ? '1' : '0') } catch {}
  }

  function applyBlock(b: HTMLElement, info: BlockInfo) {
    const i = d.blocks.findIndex(x => x.id === info.id)
    d.blocks[i] = info
    const p = txt(b)
    if (document.activeElement !== p) p.textContent = info.text
    b.toggleAttribute('data-edited', info.edited)
    if (info.edited) b.title = t('call.original', { text: info.original_text })
    else b.removeAttribute('title')
  }

  async function save(b: HTMLElement, fn: () => Promise<BlockInfo | BlockEdit>) {
    setStatus(t('call.saving'), 'busy')
    try {
      const r = await fn()
      const { edit_id, suggestions, ...info } = r as BlockEdit
      applyBlock(b, info)
      setStatus(t('call.saved'), 'ok')
      if (searching()) runSearch()
      for (const s of suggestions ?? []) offer(s, edit_id, info.id)
    } catch (e) { setStatus(t('call.save_error', { error: describeError(e) }), 'err') }
  }

  /** Devolve true se iniciou um salvamento (a busca local é refeita quando ele termina). */
  function commit(b: HTMLElement): boolean {
    clearTimeout(timers.get(Number(b.dataset.block)))
    const p = txt(b), text = norm(p.textContent ?? ''), cur = blockData(b).text
    if (text === cur) return false
    if (!text) { p.textContent = cur; return false }
    save(b, () => api.setBlockText(libraryId, blockData(b).id, text))
    return true
  }

  async function reload(keepScroll = true) {
    d = await api.callDetail(libraryId, callId)
    draw(keepScroll)
    runSearch()
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
    if (e.batch_id) return t(e.origin === 'import' ? 'history.glossary_import' : 'history.glossary_batch', { n: e.batch_size ?? 1 })
    const spk = speakerById()
    const blk = d.blocks.find(b => b.id === e.entity_id)
    switch (e.entity) {
      case 'block_text': return t('history.block_text', { seq: blk ? `#${blk.seq}` : '' })
      case 'block_speaker': return t('history.block_speaker', { seq: blk ? `#${blk.seq}` : '', from: nameOf(spk.get(Number(e.old_value))), to: nameOf(spk.get(Number(e.new_value))) })
      case 'call_title': return t('history.call_title')
      case 'speaker_name': return t('history.speaker_name', { to: e.new_value ?? t('history.cleared') })
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
        <span>${esc(t(`origin.${e.origin}`))} · ${esc(e.at.replace('T', ' '))}${e.undone_at ? ' · ' + esc(t('history.undone_tag')) : ''}</span></div>
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
      <label class="block text-sm"><span class="mb-1.5 block text-zinc-400">${esc(t('speaker.rename', { label: spk?.label ?? '' }))}</span>
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

  async function assign() {
    const r = await assignDialog({ library_id: d.library_id, client_id: d.client_id })
    if (!r) return
    try {
      const moved = await api.assign(libraryId, callId, r.libraryId, r.clientId)
      const target = store.libraries.find(l => l.id === moved.library_id)
      await hooks.reloadNav()
      toast(t('assign.done', { place: target ? libName(target) : '' }))
      location.hash = `#/call/${moved.library_id}/${moved.call_id}`
    } catch (e) { toast(describeError(e), 'err') }
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
    const body = `<p class="text-sm text-zinc-300">${esc(t('glossary.apply_summary', { n: rep.blocks_changed, r: rep.replacements }))}</p>
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

  // Sugestões de regra depois de uma edição: cartões flutuantes, sem roubar o foco da edição.
  // O padrão e a substituição são editáveis (o motor sugere só o trecho mínimo, que pode ser amplo demais).
  const panel = h('div', { id: 'suggest-panel', class: 'pointer-events-none fixed bottom-4 right-4 z-40 flex max-h-[75vh] w-[min(26rem,calc(100vw-2rem))] flex-col gap-2 overflow-y-auto' })
  document.body.appendChild(panel)
  const MAX_OFFERS = 3
  const edge = (c: string) => /[\p{L}\p{N}]/u.test(c)

  /** Quantos outros trechos (texto atual) trazem o padrão — palavra inteira, sem caixa. */
  function occurrences(pattern: string, except: number) {
    const p = norm(pattern)
    if (!p) return 0
    const re = new RegExp((edge(p[0]) ? '(?<![\\p{L}\\p{N}])' : '') + p.split(' ').map(rx).join('\\s+') + (edge(p[p.length - 1]) ? '(?![\\p{L}\\p{N}])' : ''), 'iu')
    return d.blocks.filter(b => b.id !== except && re.test(b.text)).length
  }
  const occText = (n: number) => (n ? t('suggest.occurrences', { n }) : t('suggest.occurrences_none'))

  function offer(s: BlockSuggestion, editId: number | null, blockId: number) {
    const key = `${s.pattern}\u0001${s.replacement}`
    panel.querySelectorAll<HTMLElement>('[data-offer]').forEach(c => { if (c.dataset.offer === key) c.remove() })
    const cards = panel.querySelectorAll<HTMLElement>('[data-offer]')
    if (cards.length >= MAX_OFFERS) cards[cards.length - 1].remove() // a mais antiga fica por último
    const card = h('div', { 'data-offer': key, class: 'pointer-events-auto rounded-2xl border border-violet-400/30 bg-ink-900/95 p-4 shadow-2xl backdrop-blur-md' }, `
      <div class="flex items-start justify-between gap-2">
        <p class="text-sm font-medium text-white">${esc(t('suggest.title'))}</p>
        <button type="button" data-ignore aria-label="${esc(t('suggest.ignore'))}" class="-mr-1 -mt-1 rounded-lg px-2 text-lg leading-none text-zinc-500 hover:bg-white/5 hover:text-zinc-100">×</button>
      </div>
      <p class="mt-1 text-sm text-zinc-400">‘${esc(s.pattern)}’ → ‘${esc(s.replacement)}’ · <span data-occ>${esc(occText(s.occurrences_in_call))}</span></p>
      <div class="mt-3 grid grid-cols-[1fr_auto_1fr] items-center gap-2">
        <input data-pattern maxlength="200" aria-label="${esc(t('glossary.f_pattern'))}" value="${esc(s.pattern)}" class="${inputCls} !px-2.5 !py-1.5 font-mono">
        <span class="text-zinc-600">→</span>
        <input data-replacement maxlength="500" aria-label="${esc(t('glossary.f_replacement'))}" value="${esc(s.replacement)}" class="${inputCls} !px-2.5 !py-1.5 font-mono">
      </div>
      <p class="mt-1.5 text-xs text-zinc-600">${esc(t('suggest.widen_hint'))}</p>
      <label class="mt-3 flex items-center gap-2 text-xs text-zinc-300"><input data-apply type="checkbox" ${s.occurrences_in_call ? 'checked' : ''} class="accent-violet-500"> ${esc(t('suggest.apply_now'))}</label>
      <label class="mt-1.5 flex items-center gap-2 text-xs text-zinc-400"><input data-case type="checkbox" class="accent-violet-500"> ${esc(t('glossary.f_case'))}</label>
      ${s.client ? `<p class="mt-2 text-xs text-zinc-500">${esc(t('suggest.client_is', { name: s.client.name }))}</p>` : `<p class="mt-2 text-xs text-amber-300/80">${esc(t('suggest.no_client'))}</p>`}
      <p data-err class="mt-1 min-h-4 text-xs text-rose-300"></p>
      <div class="mt-1 flex flex-wrap justify-end gap-2">
        <button type="button" data-ignore class="rounded-xl px-3 py-1.5 text-sm text-zinc-400 hover:bg-white/5 hover:text-zinc-100">${esc(t('suggest.ignore'))}</button>
        <button type="button" data-create="global" class="${btnCls.btn} !px-3 !py-1.5">${esc(t('suggest.create_global'))}</button>
        ${s.client ? `<button type="button" data-create="client" class="${btnCls.btnPrimary} !px-3 !py-1.5">${esc(t('suggest.create_client'))}</button>` : ''}
      </div>`)
    Object.assign(card, { _ctx: { s, editId, blockId } })
    panel.prepend(card)
  }

  async function createFromOffer(card: HTMLElement, scope: Scope) {
    const { s, editId } = (card as any)._ctx as { s: BlockSuggestion; editId: number | null }
    const q = <T extends HTMLElement>(sel: string) => card.querySelector<T>(sel)!
    const pattern = norm(q<HTMLInputElement>('[data-pattern]').value), replacement = norm(q<HTMLInputElement>('[data-replacement]').value)
    const err = q('[data-err]')
    err.textContent = ''
    if (!pattern) { err.textContent = t('glossary.err.pattern_empty'); return }
    if (!replacement) { err.textContent = t('glossary.err.replacement_empty'); return }
    if (replacement === pattern) { err.textContent = t('glossary.err.same'); return }
    card.querySelectorAll('button').forEach(b => (b.disabled = true))
    try {
      await api.glossaryAdd({
        scope, kind: 'replace', pattern, replacement, caseSensitive: q<HTMLInputElement>('[data-case]').checked, sourceEditId: editId,
        libraryId, clientId: scope === 'client' ? s.client?.id ?? null : null,
      })
    } catch (e) {
      err.textContent = describeGlossaryError(e)
      card.querySelectorAll('button').forEach(b => (b.disabled = false))
      return
    }
    const applyNow = q<HTMLInputElement>('[data-apply]').checked
    card.remove()
    toast(t(scope === 'client' ? 'suggest.created_client' : 'suggest.created_global'))
    if (applyNow) await applyGlossary()
  }

  panel.addEventListener('click', e => {
    const target = e.target as HTMLElement
    const card = target.closest<HTMLElement>('[data-offer]')
    if (!card) return
    if (target.closest('[data-ignore]')) card.remove()
    const c = target.closest<HTMLElement>('[data-create]')
    if (c) void createFromOffer(card, c.dataset.create as Scope)
  })
  panel.addEventListener('input', e => {
    const card = (e.target as HTMLElement).closest<HTMLElement>('[data-offer]')
    if (!card || !(e.target as HTMLElement).matches('[data-pattern]')) return
    card.querySelector('[data-occ]')!.textContent = occText(occurrences((e.target as HTMLInputElement).value, (card as any)._ctx.blockId))
  })
  panel.addEventListener('keydown', e => {
    const card = (e.target as HTMLElement).closest<HTMLElement>('[data-offer]')
    if (!card) return
    if (e.key === 'Escape') { e.preventDefault(); card.remove() }
    if (e.key === 'Enter' && (e.target as HTMLElement).matches('input[type="text"], input:not([type])')) {
      e.preventDefault()
      void createFromOffer(card, card.querySelector('[data-create="client"]') ? 'client' : 'global')
    }
  })

  function bind() {
    $('#q').addEventListener('input', runSearch)
    $('#edit-toggle').addEventListener('click', () => setEditing(!editing))
    $('#title-edit').addEventListener('click', editTitle)
    $('#history').addEventListener('click', showHistory)
    $('#assign').addEventListener('click', assign)
    $('#apply-glossary').addEventListener('click', applyGlossary)
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
  const onFocusIn = (e: FocusEvent) => {
    const p = (e.target as HTMLElement).closest?.('[data-text]') as HTMLElement | null
    if (!editing || !p || !p.querySelector('mark')) return
    p.textContent = blockData(p.closest('[data-block]')!).text // tira o destaque da busca
    const r = document.createRange(); r.selectNodeContents(p); r.collapse(false)
    const s = getSelection()!; s.removeAllRanges(); s.addRange(r)
  }
  const onInput = (e: Event) => {
    const target = e.target as HTMLElement
    const b = target.closest?.('[data-block]') as HTMLElement | null
    if (!editing || !b || !target.matches('[data-text]')) return
    setStatus(t('call.typing'), 'busy')
    const id = Number(b.dataset.block)
    clearTimeout(timers.get(id))
    timers.set(id, setTimeout(() => commit(b), 1200))
  }
  const onFocusOut = (e: FocusEvent) => {
    const target = e.target as HTMLElement
    const b = target.closest?.('[data-block]') as HTMLElement | null
    if (editing && b && target.matches('[data-text]') && !commit(b) && searching()) runSearch() // refaz o destaque removido ao focar
  }
  const onKey = (e: KeyboardEvent) => {
    const target = e.target as HTMLElement
    const p = target.closest?.('[data-text]') as HTMLElement | null
    if (p && editing) {
      if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); p.blur() }
      if (e.key === 'Escape') { e.preventDefault(); p.textContent = blockData(p.closest('[data-block]')!).text; setStatus(''); p.blur() }
      return // não roubar teclas durante a edição
    }
    if (document.querySelector('dialog[open]')) return
    const q = $<HTMLInputElement>('#q')
    if (target === q && e.key === 'Escape') { e.preventDefault(); q.value = ''; runSearch(); q.blur(); return }
    if (target.matches('input, textarea, select')) return
    if (e.key === '/') { e.preventDefault(); q.focus() }
    if (e.key === 'Escape') { q.value = ''; runSearch(); q.blur() }
    if ((e.ctrlKey || e.metaKey) && !e.shiftKey && e.key.toLowerCase() === 'z') { e.preventDefault(); undo() }
  }
  const onPaste = (e: ClipboardEvent) => {
    if (!editing || !(e.target as HTMLElement).closest?.('[data-text]')) return
    e.preventDefault()
    document.execCommand('insertText', false, norm(e.clipboardData?.getData('text/plain') ?? ''))
  }
  const onClick = (e: MouseEvent) => {
    const target = e.target as HTMLElement
    const rev = target.closest('[data-revert]')
    if (rev) { const b = rev.closest<HTMLElement>('[data-block]')!; save(b, () => api.revertBlock(libraryId, blockData(b).id)); return }
    const sb = target.closest('[data-speaker-btn]')
    if (sb && editing) { speakerMenu(sb.closest<HTMLElement>('[data-block]')!); return }
    const anchor = target.closest<HTMLAnchorElement>('[data-anchor]')
    if (anchor) { e.preventDefault(); history.replaceState(null, '', anchor.getAttribute('href')); flash(Number(anchor.closest<HTMLElement>('[data-block]')!.dataset.block)) }
  }
  const flash = (id: number) => {
    const b = el.querySelector<HTMLElement>(`#b-${id}`)
    if (!b) return
    b.scrollIntoView({ block: 'center' })
    const bubble = b.firstElementChild as HTMLElement
    bubble.removeAttribute('data-flash'); void bubble.offsetWidth; bubble.setAttribute('data-flash', '')
  }
  el.addEventListener('focusin', onFocusIn)
  el.addEventListener('input', onInput)
  el.addEventListener('focusout', onFocusOut)
  el.addEventListener('paste', onPaste)
  el.addEventListener('click', onClick)
  document.addEventListener('keydown', onKey)
  const flushPending = () => blocks().forEach(b => { if (txt(b) === document.activeElement) commit(b) })
  window.addEventListener('beforeunload', flushPending)

  draw()
  const target = Number(params.get('b'))
  const q = params.get('q')
  if (q) { $<HTMLInputElement>('#q').value = q; runSearch() }
  if (target) requestAnimationFrame(() => flash(target))

  return {
    refresh: () => reload(),
    busy: () => !!document.activeElement?.closest?.('[data-text]') || !!document.querySelector('dialog[open]') || panel.contains(document.activeElement),
    dispose: () => {
      flushPending()
      panel.remove()
      io?.disconnect()
      ro?.disconnect()
      el.style.removeProperty('--hdr')
      el.removeEventListener('focusin', onFocusIn)
      el.removeEventListener('input', onInput)
      el.removeEventListener('focusout', onFocusOut)
      el.removeEventListener('paste', onPaste)
      el.removeEventListener('click', onClick)
      document.removeEventListener('keydown', onKey)
      window.removeEventListener('beforeunload', flushPending)
    },
  }
}

