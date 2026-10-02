import { api, type SearchHit } from '../api'
import { t } from '../i18n'
import { libName, store, type View } from '../store'
import { callTitle, esc, fmtClock, fmtDate, fmtTime, markSnippet } from '../util'

export async function renderSearch(el: HTMLElement, query: string): Promise<View> {
  const input = document.getElementById('global-q') as HTMLInputElement | null
  if (input && input.value !== query) input.value = query
  const draw = async () => {
    const hits = query.trim() ? await api.search(query) : []
    const byCall = new Map<string, SearchHit[]>()
    for (const h of hits) {
      const k = `${h.library_id}:${h.call_id}`
      byCall.set(k, [...(byCall.get(k) ?? []), h])
    }
    const groups = [...byCall.values()].map(hs => {
      const c = hs[0]
      const lib = store.libraries.find(l => l.id === c.library_id)
      const titleHit = hs.find(h => h.block_id === null)
      const title = titleHit && c.call_title ? markSnippet(titleHit.snippet) : esc(callTitle({ title: c.call_title, started_at: c.started_at }))
      const items = hs.filter(h => h.block_id !== null).map(h => `
        <a href="#/call/${h.library_id}/${h.call_id}?b=${h.block_id}&q=${encodeURIComponent(query)}" class="flex gap-3 rounded-xl px-3 py-2 text-sm hover:bg-white/5">
          <span class="shrink-0 font-mono text-xs leading-6 text-zinc-500">${fmtTime(h.t_start ?? 0)}</span>
          <span class="text-zinc-300">${markSnippet(h.snippet)}</span></a>`).join('')
      return `<section class="rounded-2xl border border-white/10 bg-ink-900/80 p-4">
        <a href="#/call/${c.library_id}/${c.call_id}" class="block px-3">
          <h3 class="text-base font-semibold text-white hover:text-violet-200">${title}</h3>
          <p class="text-xs text-zinc-500">${esc(fmtDate(c.started_at))} · ${esc(fmtClock(c.started_at))}${lib ? ' · ' + esc(libName(lib)) : ''}</p></a>
        ${items ? `<div class="mt-2">${items}</div>` : ''}</section>`
    }).join('')
    el.innerHTML = `<div class="mx-auto max-w-4xl px-6 py-10">
      <h1 class="text-2xl font-semibold tracking-tight text-white">${esc(t('search.title', { q: query }))}</h1>
      <p class="mt-1 text-sm text-zinc-500">${esc(t('search.count', { n: hits.length }))} · ${esc(t('search.hint'))}</p>
      <div class="mt-6 space-y-3">${groups || `<p class="mt-10 text-center text-sm text-zinc-600">${esc(t('list.no_match'))}</p>`}</div></div>`
  }
  await draw()
  return { refresh: draw }
}
