import { api, on, pickFolder, type ImportItem, type ImportProgress, type ImportReport } from '../api'
import { t } from '../i18n'
import { hooks, libName, store, type View } from '../store'
import { btnCls, describeError, inputCls } from '../dialogs'
import { esc, fmtBytes, toast } from '../util'

const statusCls: Record<ImportItem['status'], string> = {
  new: 'text-emerald-300 bg-emerald-400/10',
  updated: 'text-sky-300 bg-sky-400/10',
  unchanged: 'text-zinc-400 bg-white/5',
  skipped: 'text-amber-300 bg-amber-400/10',
}

function reportTable(r: ImportReport) {
  const rows = r.items.map(i => `<tr class="border-t border-white/5">
    <td class="py-2 pr-3 font-mono text-xs text-zinc-300">${esc(i.key)}</td>
    <td class="py-2 pr-3"><span class="rounded-full px-2 py-0.5 text-xs ${statusCls[i.status]}">${esc(t(`import.status.${i.status}`))}</span></td>
    <td class="py-2 pr-3 text-xs text-zinc-400">${i.versions_added.length ? esc(i.versions_added.map(v => t('import.versions', { v })).join(', ')) : ''}${i.edits_applied ? ' · ' + esc(t('import.edits', { n: i.edits_applied })) : ''}${i.glossary_replacements > 0 ? ' · ' + esc(t('import.glossary', { n: i.glossary_replacements })) : ''}</td>
    <td class="py-2 pr-3 text-xs text-zinc-400">${[
      ...i.audio_converted.map(a => `✓ ${a}`),
      ...i.audio_pending.map(a => `${r.dry_run ? '→' : '…'} ${a}`),
      ...i.audio_errors.map(a => `<span class="text-rose-300">✗ ${esc(a)}</span>`),
    ].join(' ')}</td>
    <td class="py-2 text-xs text-zinc-500">${esc(i.reason ?? '')}</td></tr>`).join('')
  return `<table class="w-full text-left"><thead class="text-[11px] uppercase tracking-wider text-zinc-600"><tr>
    <th class="pb-2">${esc(t('import.col.call'))}</th><th class="pb-2">${esc(t('import.col.status'))}</th>
    <th class="pb-2">${esc(t('import.col.versions'))}</th><th class="pb-2">${esc(t('import.col.audio'))}</th><th class="pb-2">${esc(t('import.col.note'))}</th>
    </tr></thead><tbody>${rows}</tbody></table>`
}

export async function renderImport(el: HTMLElement): Promise<View> {
  let folder = ''
  let running = false
  const unlisten: (() => void)[] = []
  const libs = store.libraries.filter(l => l.available)
  const inbox = store.boot.inbox_id

  el.innerHTML = `<div class="mx-auto max-w-5xl px-6 py-10">
    <h1 class="text-3xl font-semibold tracking-tight text-white">${esc(t('import.title'))}</h1>
    <p class="mt-2 max-w-3xl text-sm text-zinc-500">${esc(t('import.intro'))}</p>
    <div class="mt-6 flex gap-2">
      <input id="folder" class="${inputCls}" placeholder="${esc(t('import.folder_ph'))}">
      <button id="pick" type="button" class="${btnCls.btn} shrink-0">${esc(t('common.choose'))}</button>
      <button id="preview" type="button" class="${btnCls.btn} shrink-0">${esc(t('import.preview'))}</button>
    </div>
    <div id="preview-box" class="mt-6"></div>
    <section class="mt-12 rounded-2xl border border-white/10 bg-ink-900/60 p-5">
      <h2 class="text-sm font-semibold text-white">${esc(t('import.reclaim_title'))}</h2>
      <div id="reclaim" class="mt-2 text-sm text-zinc-500">…</div>
    </section></div>`

  const $ = <T extends HTMLElement>(s: string) => el.querySelector<T>(s)!
  const box = $('#preview-box')

  async function loadReclaim() {
    const r = await api.reclaimable()
    $('#reclaim').innerHTML = r.files.length
      ? `<p>${esc(t('import.reclaim_total', { size: fmtBytes(r.total_bytes), n: r.files.length }))}</p>
         <details class="mt-2"><summary class="cursor-pointer text-xs text-zinc-500">${esc(t('import.reclaim_list'))}</summary>
         <ul class="mt-2 space-y-0.5 font-mono text-[11px] text-zinc-500">${r.files.map(f => `<li>${esc(fmtBytes(f.size))} · ${esc(f.path)}</li>`).join('')}</ul></details>
         <p class="mt-2 text-xs text-zinc-600">${esc(t('import.reclaim_hint'))}</p>`
      : esc(t('import.reclaim_none'))
  }

  async function preview() {
    folder = $<HTMLInputElement>('#folder').value.trim()
    if (!folder) return
    box.innerHTML = `<p class="text-sm text-zinc-500">${esc(t('import.scanning'))}</p>`
    try {
      const r = await api.importPreview([folder], null)
      const actionable = r.items.some(i => i.status === 'new' || i.status === 'updated' || i.audio_pending.length)
      const libOpts = libs.map(l => `<option value="${l.id}" ${l.id === inbox ? 'selected' : ''}>${esc(libName(l))}</option>`).join('')
      box.innerHTML = `<div class="rounded-2xl border border-white/10 bg-ink-900/80 p-5">
        <div class="overflow-x-auto">${reportTable(r)}</div>
        ${actionable ? `<div class="mt-5 flex flex-wrap items-end gap-3 border-t border-white/10 pt-4">
          <label class="text-sm"><span class="mb-1 block text-zinc-400">${esc(t('import.target'))}</span><select id="target" class="${inputCls}">${libOpts}</select></label>
          <label class="text-sm" id="client-wrap" hidden><span class="mb-1 block text-zinc-400">${esc(t('assign.client'))}</span><select id="client" class="${inputCls}"></select></label>
          <label class="flex items-center gap-2 text-sm text-zinc-400"><input id="audio" type="checkbox" checked class="accent-violet-500"> ${esc(t('import.convert_audio'))}</label>
          <button id="go" type="button" class="${btnCls.btnPrimary} ml-auto">${esc(t('import.start'))}</button>
        </div><p class="mt-2 text-xs text-zinc-600">${esc(t('import.target_hint'))}</p>` : `<p class="mt-4 text-sm text-zinc-500">${esc(t('import.nothing'))}</p>`}
        <div id="progress" class="mt-4"></div></div>`
      const target = el.querySelector<HTMLSelectElement>('#target')
      target?.addEventListener('change', async () => {
        const lib = libs.find(l => l.id === Number(target.value))!
        const wrap = $('#client-wrap')
        wrap.hidden = lib.kind !== 'company'
        const cs = lib.kind === 'company' ? await api.clients(lib.id) : []
        $<HTMLSelectElement>('#client').innerHTML = `<option value="">${esc(t('assign.no_client'))}</option>` + cs.map(c => `<option value="${c.id}">${esc(c.name)}</option>`).join('')
      })
      el.querySelector('#go')?.addEventListener('click', start)
    } catch (e) {
      box.innerHTML = `<p class="text-sm text-rose-300">${esc(describeError(e))}</p>`
    }
  }

  async function start() {
    if (running) return
    running = true
    const go = $<HTMLButtonElement>('#go')
    go.disabled = true
    const libraryId = Number($<HTMLSelectElement>('#target').value)
    const clientVal = el.querySelector<HTMLSelectElement>('#client')?.value
    const clientId = clientVal ? Number(clientVal) : null
    $('#progress').innerHTML = `<div class="h-2 overflow-hidden rounded-full bg-white/5"><div id="bar" class="h-full w-0 rounded-full bg-violet-500 transition-[width]"></div></div><p id="ptext" class="mt-2 text-xs text-zinc-500"></p>`
    try {
      await api.importStart([folder], libraryId === inbox ? null : libraryId, clientId, $<HTMLInputElement>('#audio').checked)
    } catch (e) {
      running = false; go.disabled = false
      toast(describeError(e), 'err')
    }
  }

  unlisten.push(await on<ImportProgress>('import-progress', p => {
    const bar = el.querySelector<HTMLElement>('#bar'), text = el.querySelector<HTMLElement>('#ptext')
    if (!bar || !text) return
    if (p.stage === 'call') { text.textContent = t('import.progress_call', { i: p.index + 1, n: p.total, key: p.key }) }
    if (p.stage === 'audio') {
      const frac = (p.index + p.done / Math.max(1, p.of)) / Math.max(1, p.total)
      bar.style.width = `${Math.round(frac * 100)}%`
      text.textContent = t('import.progress_audio', { i: p.index + 1, n: p.total, key: p.key, track: p.track, pct: Math.round((p.done * 100) / Math.max(1, p.of)) })
    }
  }))
  unlisten.push(await on<{ ok: boolean; report?: ImportReport; error?: unknown }>('import-done', async r => {
    running = false
    if (!r.ok) { toast(describeError(r.error), 'err'); return }
    toast(t('import.done'))
    box.innerHTML = `<div class="rounded-2xl border border-white/10 bg-ink-900/80 p-5"><h2 class="mb-3 text-sm font-semibold text-white">${esc(t('import.result'))}</h2><div class="overflow-x-auto">${reportTable(r.report!)}</div></div>`
    await hooks.reloadNav()
    await loadReclaim()
  }))

  $('#pick').addEventListener('click', async () => {
    const p = await pickFolder(t('import.folder_ph'))
    if (p) { $<HTMLInputElement>('#folder').value = p; preview() }
  })
  $('#preview').addEventListener('click', preview)
  $('#folder').addEventListener('keydown', e => { if (e.key === 'Enter') preview() })
  await loadReclaim()
  return { dispose: () => unlisten.forEach(u => u()), busy: () => running }
}
