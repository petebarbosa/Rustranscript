import './styles.css'
import { api, on, inTauri, openExternal, isBarWindow, REC_EVENTS, type FinalizeDone } from './api'
import { hooks, store, type View } from './store'
import { resolveLang, setLang, t } from './i18n'
import { esc, debounce, fmtNumber, fmtTime, toast } from './util'
import { elapsedNow, hub, isRecording, startHub, subscribe } from './rec'
import { finalizeToast, initRecovery } from './recovery'
import { addLibraryDialog, addClientDialog } from './dialogs'
import { renderList } from './views/list'
import { renderCall } from './views/call'
import { renderSearch } from './views/search'
import { renderSettings } from './views/settings'
import { renderImport } from './views/import'
import { renderGlossary } from './views/glossary'
import { renderRecord } from './views/record'
import { renderBar } from './views/bar'

let view: View = {}
const app = document.getElementById('app')!

async function loadNav() {
  store.libraries = await api.libraries()
  store.clients = new Map()
  await Promise.all(
    store.libraries
      .filter(l => l.kind === 'company' && l.available)
      .map(async l => store.clients.set(l.id, await api.clients(l.id))),
  )
  renderSidebar()
}

const AUTHOR_URL = 'https://github.com/petebarbosa'

/** Rodapé "Licença MIT · Desenvolvido por <link>": o texto traduzido é escapado e só o nome vira link. */
function creditHtml(): string {
  const link = `<a id="author-link" href="${AUTHOR_URL}" target="_blank" rel="noopener noreferrer" class="whitespace-nowrap text-zinc-500 underline decoration-zinc-700 underline-offset-2 hover:text-violet-300">Pedro Barbosa</a>`
  return esc(t('app.credit', { name: '\u0001' })).replace('\u0001', link)
}

function shell() {
  app.innerHTML = `
  <div class="flex h-screen">
    <aside class="flex w-64 shrink-0 flex-col border-r border-white/10 bg-ink-950/70">
      <div class="px-4 pt-5">
        <a href="#/" class="block text-lg font-semibold tracking-tight text-white">${esc(t('app.name'))}</a>
        <input id="global-q" type="search" autocomplete="off" placeholder="${esc(t('nav.search'))}"
          class="mt-4 w-full rounded-xl border border-white/10 bg-ink-900 px-3 py-2 text-sm text-zinc-100 placeholder:text-zinc-600 focus:border-violet-400/60 focus:outline-none focus:ring-2 focus:ring-violet-400/20">
        <a id="rec-link" href="#/record" data-route="record" class="mt-3 flex items-center justify-between gap-2 rounded-xl border border-rose-400/40 bg-rose-500/15 px-3 py-2 text-sm font-semibold text-white hover:bg-rose-500/25 aria-[current=page]:ring-2 aria-[current=page]:ring-rose-400/40"></a>
      </div>
      <nav id="nav" class="mt-4 flex-1 space-y-0.5 overflow-y-auto px-2 pb-4 text-sm"></nav>
      <div class="space-y-0.5 border-t border-white/10 p-2 text-sm">
        <a href="#/import" data-route="import" class="nav-item flex items-center gap-2 rounded-lg px-3 py-1.5 text-zinc-400 hover:bg-white/5 hover:text-zinc-100">⇪ ${esc(t('nav.import'))}</a>
        <a href="#/glossary" data-route="glossary" class="nav-item flex items-center gap-2 rounded-lg px-3 py-1.5 text-zinc-400 hover:bg-white/5 hover:text-zinc-100"><span class="w-4 text-center text-xs font-semibold">Aa</span> ${esc(t('nav.glossary'))}</a>
        <a href="#/settings" data-route="settings" class="nav-item flex items-center gap-2 rounded-lg px-3 py-1.5 text-zinc-400 hover:bg-white/5 hover:text-zinc-100">⚙ ${esc(t('nav.settings'))}</a>
      </div>
      <p class="border-t border-white/10 px-4 py-3 text-xs text-zinc-600">${creditHtml()}</p>
    </aside>
    <main id="view" class="relative min-w-0 flex-1 overflow-y-auto"></main>
  </div>`
  document.getElementById('author-link')!.addEventListener('click', e => {
    if (!inTauri) return // no navegador o target="_blank" já resolve
    e.preventDefault()
    void openExternal(AUTHOR_URL).catch(() => {})
  })
  const q = document.getElementById('global-q') as HTMLInputElement
  const go = () => { const v = q.value.trim(); if (v) location.hash = `#/search?q=${encodeURIComponent(v)}` }
  q.addEventListener('input', debounce(go, 350))
  q.addEventListener('keydown', e => { if (e.key === 'Enter') go() })
  document.getElementById('nav')!.addEventListener('click', async e => {
    const btn = (e.target as HTMLElement).closest<HTMLElement>('[data-action]')
    if (!btn) return
    e.preventDefault()
    if (btn.dataset.action === 'add-library') {
      if (await addLibraryDialog()) await loadNav()
    } else if (btn.dataset.action === 'add-client') {
      const c = await addClientDialog(Number(btn.dataset.lib))
      if (c) { await loadNav(); location.hash = `#/lib/${c.library_id}/client/${c.id}` }
    }
  })
}

/** Botão "Gravar" da barra lateral: ocioso = ponto + rótulo; gravando = ponto pulsando + cronômetro; finalizando = aviso. */
function paintRecLink() {
  const a = document.getElementById('rec-link')
  if (!a) return
  const rec = isRecording()
  const fin = hub.status?.finalizing.length ?? 0
  const dot = `<span class="h-2.5 w-2.5 shrink-0 rounded-full bg-rose-500 ${rec ? 'animate-pulse' : ''}"></span>`
  a.innerHTML = rec
    ? `<span class="flex items-center gap-2">${dot}${esc(t('record.recording'))}</span><span id="rec-clock" class="font-mono text-sm tabular-nums text-rose-100">${fmtTime(elapsedNow())}</span>`
    : `<span class="flex items-center gap-2">${dot}${esc(t('nav.record'))}</span>${fin ? `<span class="rounded-full bg-amber-400/15 px-2 py-0.5 text-[11px] font-medium text-amber-200">${esc(t('record.finalizing'))}</span>` : ''}`
  a.title = rec ? t('record.recording') : ''
}

function navLink(href: string, label: string, count: number | null, indent = false) {
  return `<a href="${href}" data-href="${href}" class="nav-item flex items-center justify-between gap-2 rounded-lg ${indent ? 'pl-6 pr-3' : 'px-3'} py-1.5 text-zinc-400 hover:bg-white/5 hover:text-zinc-100">
    <span class="truncate">${esc(label)}</span>${count == null ? '' : `<span class="shrink-0 text-xs tabular-nums text-zinc-600">${fmtNumber(count)}</span>`}</a>`
}

function renderSidebar() {
  const nav = document.getElementById('nav')
  if (!nav) return
  const total = store.libraries.reduce((n, l) => n + l.call_count, 0)
  const unassigned = store.libraries.reduce((n, l) => n + l.unassigned_count, 0)
  const companies = store.libraries.filter(l => l.kind === 'company')
  const libsHtml = companies.map(l => {
    if (!l.available) {
      return `<div class="px-3 py-1.5 text-zinc-600" title="${esc(t('nav.unavailable', { path: l.path }))}">${esc(l.name)} · ${esc(t('nav.offline'))}</div>`
    }
    const clients = store.clients.get(l.id) ?? []
    const noClient = l.unassigned_count
    return `<div class="mt-1">
      ${navLink(`#/lib/${l.id}`, l.name, l.call_count)}
      ${clients.map(c => navLink(`#/lib/${l.id}/client/${c.id}`, c.name, c.call_count, true)).join('')}
      ${noClient ? navLink(`#/lib/${l.id}/unassigned`, t('nav.no_client'), noClient, true) : ''}
      <button type="button" data-action="add-client" data-lib="${l.id}" class="block w-full rounded-lg py-1 pl-6 text-left text-xs text-zinc-600 hover:text-violet-300">+ ${esc(t('nav.add_client'))}</button>
    </div>`
  }).join('')
  nav.innerHTML = `
    ${navLink('#/', t('nav.all'), total)}
    ${navLink('#/unclassified', t('nav.unclassified'), unassigned)}
    <div class="mt-5 flex items-center justify-between px-3 pb-1 text-[11px] font-semibold uppercase tracking-wider text-zinc-600">
      <span>${esc(t('nav.companies'))}</span>
      <button type="button" data-action="add-library" title="${esc(t('nav.add_company'))}" class="rounded px-1.5 text-base leading-none text-zinc-500 hover:bg-white/5 hover:text-violet-300">+</button>
    </div>
    ${libsHtml || `<p class="px-3 py-1 text-xs text-zinc-600">${esc(t('nav.no_companies'))}</p>`}`
  markCurrent()
}

function markCurrent() {
  const hash = (location.hash || '#/').split('?')[0]
  document.querySelectorAll<HTMLElement>('.nav-item, #rec-link').forEach(a => {
    const href = a.dataset.href ?? a.getAttribute('href')
    a.toggleAttribute('aria-current', href === hash)
    if (href === hash) a.setAttribute('aria-current', 'page')
  })
}

async function route() {
  view.dispose?.()
  view = {}
  const el = document.getElementById('view')!
  el.scrollTop = 0
  const [path, query] = (location.hash.slice(1) || '/').split('?')
  const params = new URLSearchParams(query ?? '')
  const parts = path.split('/').filter(Boolean)
  markCurrent()
  const gq = document.getElementById('global-q') as HTMLInputElement | null
  if (gq && parts[0] !== 'search') gq.value = ''
  try {
    if (parts[0] === 'call') view = await renderCall(el, Number(parts[1]), Number(parts[2]), params)
    else if (parts[0] === 'search') view = await renderSearch(el, params.get('q') ?? '')
    else if (parts[0] === 'settings') view = await renderSettings(el)
    else if (parts[0] === 'glossary') view = await renderGlossary(el, params)
    else if (parts[0] === 'import') view = await renderImport(el)
    else if (parts[0] === 'record') view = await renderRecord(el)
    else if (parts[0] === 'unclassified') view = await renderList(el, { kind: 'unclassified' })
    else if (parts[0] === 'lib' && parts[2] === 'client') view = await renderList(el, { kind: 'client', libraryId: Number(parts[1]), clientId: Number(parts[3]) })
    else if (parts[0] === 'lib' && parts[2] === 'unassigned') view = await renderList(el, { kind: 'lib-unassigned', libraryId: Number(parts[1]) })
    else if (parts[0] === 'lib') view = await renderList(el, { kind: 'library', libraryId: Number(parts[1]) })
    else view = await renderList(el, { kind: 'all' })
  } catch (e: any) {
    el.innerHTML = `<div class="m-10 rounded-2xl border border-rose-400/30 bg-rose-400/5 p-6 text-rose-200">${esc(t('error.generic'))}: ${esc(e?.detail ?? String(e))}</div>`
  }
}

let pending = false
async function onExternalChange() {
  if (view.busy?.()) { pending = true; return }
  pending = false
  await loadNav()
  await view.refresh?.()
}
document.addEventListener('focusout', () => { if (pending) setTimeout(onExternalChange, 50) })

async function main() {
  store.boot = await api.bootstrap()
  const saved = store.boot.settings.language
  const l = resolveLang(saved ?? store.boot.system_language)
  setLang(l)
  // fixa o idioma detectado na 1ª execução, para CLI e janela concordarem
  if (!saved) api.setSetting('language', l).catch(() => {})
  // A mini barra é outra janela do mesmo front-end: só idioma e a própria tela, sem shell nem navegação.
  if (isBarWindow()) { await renderBar(app); return }
  hooks.reloadNav = loadNav
  shell()
  await loadNav()
  window.addEventListener('hashchange', route)
  // gravação: estado compartilhado + indicador na barra lateral; fim de finalização e recuperação valem em qualquer tela
  await startHub()
  paintRecLink()
  subscribe('state', paintRecLink)
  setInterval(() => { const c = document.getElementById('rec-clock'); if (c && isRecording()) c.textContent = fmtTime(elapsedNow()) }, 500)
  await route()
  await on('data-changed', () => onExternalChange())
  await on<FinalizeDone>(REC_EVENTS.finalizeDone, finalizeToast)
  await initRecovery()
}

main().catch(e => {
  app.innerHTML = `<pre class="m-6 whitespace-pre-wrap text-rose-300">${esc(e?.detail ?? String(e))}</pre>`
  toast(String(e?.detail ?? e), 'err')
})
