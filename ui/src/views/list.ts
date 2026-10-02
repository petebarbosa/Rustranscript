import { api, type CallSummary } from '../api'
import { t } from '../i18n'
import { libName, store, type View } from '../store'
import { callTitle, esc, fmtClock, fmtDate, fmtDuration, fmtNumber, fold } from '../util'

export type Scope =
  | { kind: 'all' }
  | { kind: 'unclassified' }
  | { kind: 'library'; libraryId: number }
  | { kind: 'lib-unassigned'; libraryId: number }
  | { kind: 'client'; libraryId: number; clientId: number }

function heading(scope: Scope) {
  const lib = 'libraryId' in scope ? store.libraries.find(l => l.id === scope.libraryId) : undefined
  switch (scope.kind) {
    case 'all': return { title: t('list.all'), sub: '' }
    case 'unclassified': return { title: t('nav.unclassified'), sub: t('list.unclassified_hint') }
    case 'library': return { title: lib ? libName(lib) : '', sub: lib?.path ?? '' }
    case 'lib-unassigned': return { title: t('nav.no_client'), sub: lib ? libName(lib) : '' }
    case 'client': {
      const c = store.clients.get(scope.libraryId)?.find(c => c.id === scope.clientId)
      return { title: c?.name ?? '', sub: lib ? libName(lib) : '' }
    }
  }
}

async function load(scope: Scope): Promise<CallSummary[]> {
  switch (scope.kind) {
    case 'all': return api.calls(null, null, false)
    case 'unclassified': return api.calls(null, null, true)
    case 'library': return api.calls(scope.libraryId, null, false)
    case 'lib-unassigned': return api.calls(scope.libraryId, null, true)
    case 'client': return api.calls(scope.libraryId, scope.clientId, false)
  }
}

function card(c: CallSummary, showPlace: boolean) {
  const lib = store.libraries.find(l => l.id === c.library_id)
  const place = lib?.kind === 'inbox' ? t('nav.unclassified') : [lib?.name, c.client_name].filter(Boolean).join(' · ')
  const chip = (s: string, cls = '') => `<span class="rounded-full bg-white/[0.04] px-2.5 py-0.5 ${cls}">${esc(s)}</span>`
  // gravação sem transcrição ainda (pending/running/failed): selo no lugar de palavras/prévia
  const tr = c.transcription_state
  const pending = tr !== 'done'
  const badge = pending
    ? chip(t(`transcription.${tr}`), tr === 'failed' ? 'text-rose-300 bg-rose-400/10' : tr === 'running' ? 'text-sky-300 bg-sky-400/10' : 'text-amber-300 bg-amber-400/10')
    : ''
  const search = esc(fold(`${callTitle(c)} ${c.preview} ${place}`))
  return `<a href="#/call/${c.library_id}/${c.id}" data-card data-search="${search}"
    class="group block rounded-2xl border border-white/10 bg-ink-900/80 p-5 transition hover:-translate-y-0.5 hover:border-violet-400/40 hover:bg-ink-800">
    <div class="flex items-start justify-between gap-4">
      <h3 class="text-lg font-semibold tracking-tight text-white group-hover:text-violet-200 ${c.title ? '' : 'italic text-zinc-300'}">${esc(callTitle(c))}</h3>
      <span class="text-zinc-600 transition group-hover:translate-x-0.5 group-hover:text-violet-300">→</span>
    </div>
    <div class="mt-2 flex flex-wrap gap-2 text-xs text-zinc-400">
      ${chip(fmtClock(c.started_at), 'font-mono')}
      ${chip(fmtDuration(c.duration_s))}
      ${badge}
      ${pending ? '' : chip(t('list.words', { n: c.words, count: fmtNumber(c.words) }))}
      ${showPlace && place ? chip(place, 'text-zinc-500') : ''}
      ${c.edited_blocks ? chip(t('list.edited', { n: c.edited_blocks }), 'text-amber-300 bg-amber-400/10') : ''}
      ${c.versions > 1 ? chip(t('list.versions', { n: c.versions })) : ''}
    </div>
    <p class="mt-3 line-clamp-2 text-sm leading-relaxed text-zinc-500 ${pending ? 'italic' : ''}">${esc(pending ? t(`transcription.${tr}_hint`, { error: c.transcription_error ?? '' }) : c.preview)}</p></a>`
}

export async function renderList(el: HTMLElement, scope: Scope): Promise<View> {
  const draw = async () => {
    const calls = await load(scope)
    const { title, sub } = heading(scope)
    const total = calls.reduce((n, c) => n + c.duration_s, 0)
    const byDate = new Map<string, CallSummary[]>()
    for (const c of calls) {
      const d = c.started_at.slice(0, 10)
      byDate.set(d, [...(byDate.get(d) ?? []), c])
    }
    const showPlace = scope.kind === 'all' || scope.kind === 'unclassified' || scope.kind === 'library'
    const groups = [...byDate.entries()].map(([d, cs]) => `
      <section data-group class="mt-10">
        <h2 class="mb-3 text-xs font-semibold uppercase tracking-wider text-zinc-500">${esc(fmtDate(d))}</h2>
        <div class="grid gap-3 xl:grid-cols-2">${cs.map(c => card(c, showPlace)).join('')}</div>
      </section>`).join('')
    const q = el.querySelector<HTMLInputElement>('#list-q')?.value ?? ''
    el.innerHTML = `<div class="mx-auto max-w-5xl px-6 py-10">
      <h1 class="text-3xl font-semibold tracking-tight text-white">${esc(title)}</h1>
      ${sub ? `<p class="mt-1 text-sm text-zinc-500">${esc(sub)}</p>` : ''}
      <p class="mt-2 text-sm text-zinc-500">${calls.length ? esc(t('list.stats', { n: calls.length, count: fmtNumber(calls.length), duration: fmtDuration(total) })) : ''}</p>
      ${calls.length ? `<input id="list-q" type="search" autocomplete="off" placeholder="${esc(t('list.filter'))}" value="${esc(q)}"
        class="mt-6 w-full rounded-xl border border-white/10 bg-ink-900 px-4 py-2.5 text-sm text-zinc-100 placeholder:text-zinc-600 focus:border-violet-400/60 focus:outline-none focus:ring-2 focus:ring-violet-400/20">` : ''}
      ${groups || `<div class="mt-12 rounded-2xl border border-dashed border-white/10 p-10 text-center text-zinc-500">
        ${esc(t(scope.kind === 'all' ? 'list.empty_all' : 'list.empty'))}
        ${scope.kind === 'all' ? `<a href="#/import" class="mt-3 block text-violet-300 hover:underline">${esc(t('nav.import'))} →</a>` : ''}</div>`}
      <p id="no-match" hidden class="mt-10 text-center text-sm text-zinc-600">${esc(t('list.no_match'))}</p>
    </div>`
    const input = el.querySelector<HTMLInputElement>('#list-q')
    const run = () => {
      const v = fold(input?.value.trim() ?? '')
      let n = 0
      el.querySelectorAll<HTMLElement>('[data-card]').forEach(c => { const hit = !v || c.dataset.search!.includes(v); c.hidden = !hit; if (hit) n++ })
      el.querySelectorAll<HTMLElement>('[data-group]').forEach(g => (g.hidden = !g.querySelector('[data-card]:not([hidden])')))
      el.querySelector<HTMLElement>('#no-match')!.hidden = n > 0 || !calls.length
    }
    input?.addEventListener('input', run)
    if (q) run()
  }
  await draw()
  const onKey = (e: KeyboardEvent) => {
    const input = el.querySelector<HTMLInputElement>('#list-q')
    if (!input || (e.target as HTMLElement).matches('input, textarea, [contenteditable]')) return
    if (e.key === '/') { e.preventDefault(); input.focus() }
  }
  document.addEventListener('keydown', onKey)
  return { refresh: draw, dispose: () => document.removeEventListener('keydown', onKey) }
}
