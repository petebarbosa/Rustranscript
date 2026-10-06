// Configurações > áudio das chamadas: lista as chamadas que ainda têm áudio no disco, com o tamanho de cada uma,
// para apagar várias de uma vez (#24). A transcrição de cada chamada fica.
import { api, type AudioEntry } from '../api'
import { audioError, confirmAudioDelete } from '../audio'
import { t } from '../i18n'
import { btnCls } from '../dialogs'
import { callTitle, esc, fmtBytes, fmtDate, toast } from '../util'

export function mountAudioStorage(box: HTMLElement): () => void {
  let alive = true
  let entries: AudioEntry[] = []
  const selected = new Set<string>()
  const keyOf = (e: AudioEntry) => `${e.library_id}:${e.call_id}`
  const can = (e: AudioEntry) => e.blocked === null
  const bytesOf = (list: AudioEntry[]) => list.reduce((n, e) => n + e.bytes, 0)

  const row = (e: AudioEntry) => `<li class="flex flex-wrap items-center gap-x-3 gap-y-1 rounded-xl border border-white/10 bg-ink-900/60 px-4 py-2.5 ${can(e) ? '' : 'opacity-70'}">
    <input type="checkbox" data-pick="${keyOf(e)}" ${can(e) ? '' : 'disabled'} ${selected.has(keyOf(e)) ? 'checked' : ''} aria-label="${esc(callTitle(e))}" class="accent-violet-500">
    <div class="min-w-0 basis-56 flex-1">
      <a href="#/call/${e.library_id}/${e.call_id}" class="block truncate text-sm font-medium text-zinc-100 hover:text-violet-300">${esc(callTitle(e))}</a>
      <p class="truncate text-xs text-zinc-500">${esc(fmtDate(e.started_at))}${e.client_name ? ' · ' + esc(e.client_name) : ''}</p></div>
    ${e.blocked ? `<span class="rounded-full bg-amber-400/10 px-2 py-0.5 text-[11px] text-amber-300">${esc(t(`audio.blocked.${e.blocked}`))}</span>` : ''}
    <span class="w-20 shrink-0 text-right font-mono text-xs text-zinc-300">${esc(fmtBytes(e.bytes))}</span></li>`

  function draw(total: number) {
    const picked = entries.filter(e => selected.has(keyOf(e)))
    const deletable = entries.filter(can)
    box.innerHTML = `<h2 class="text-sm font-semibold text-white">${esc(t('audio.storage_title'))}</h2>
      <p class="mt-1 text-xs text-zinc-600">${esc(t('audio.storage_hint'))}</p>
      ${entries.length ? `<p class="mt-3 text-sm text-zinc-400">${esc(t('audio.storage_total', { size: fmtBytes(total), n: entries.length }))}</p>
        <div class="mt-3 flex flex-wrap items-center gap-3">
          <label class="flex items-center gap-2 text-xs text-zinc-400"><input type="checkbox" data-all ${deletable.length && picked.length === deletable.length ? 'checked' : ''} ${deletable.length ? '' : 'disabled'} class="accent-violet-500"> ${esc(t('audio.select_all'))}</label>
          <span class="text-xs text-zinc-500">${picked.length ? esc(t('audio.selected', { n: picked.length, size: fmtBytes(bytesOf(picked)) })) : ''}</span>
          <button type="button" data-bulk ${picked.length ? '' : 'disabled'} class="${btnCls.btn} ml-auto hover:!border-rose-400/50 disabled:cursor-not-allowed disabled:opacity-40">${esc(t('audio.delete_selected'))}</button></div>
        <ul id="audio-list" class="mt-3 space-y-2">${entries.map(row).join('')}</ul>`
      : `<p class="mt-3 text-sm text-zinc-600">${esc(t('audio.storage_none'))}</p>`}`
    box.querySelectorAll<HTMLInputElement>('[data-pick]').forEach(c => c.addEventListener('change', () => {
      if (c.checked) selected.add(c.dataset.pick!); else selected.delete(c.dataset.pick!)
      draw(total)
    }))
    box.querySelector<HTMLInputElement>('[data-all]')?.addEventListener('change', ev => {
      selected.clear()
      if ((ev.target as HTMLInputElement).checked) deletable.forEach(e => selected.add(keyOf(e)))
      draw(total)
    })
    box.querySelector('[data-bulk]')?.addEventListener('click', () => void deleteSelected(picked))
  }

  async function load() {
    try {
      const r = await api.audioList()
      if (!alive) return
      entries = r.calls
      for (const k of [...selected]) if (!entries.some(e => keyOf(e) === k && can(e))) selected.delete(k)
      draw(r.total_bytes)
    } catch (e) { if (alive) box.innerHTML = `<p class="text-sm text-rose-300">${esc(audioError(e))}</p>` }
  }

  /** Uma exclusão por chamada; o que falhar (virou tarefa na fila, p.ex.) é contado e a lista recarrega. */
  async function deleteSelected(picked: AudioEntry[]) {
    const r = await confirmAudioDelete(t('audio.bulk_title', { n: picked.length }), bytesOf(picked), async () => {
      let ok = 0, fail = 0, freed = 0
      for (const e of picked) {
        try { freed += (await api.audioDelete(e.library_id, e.call_id, false)).bytes; ok++ } catch { fail++ }
      }
      return { ok, fail, freed }
    })
    if (!r) return
    selected.clear()
    await load()
    toast(r.ok ? t('audio.bulk_done', { n: r.ok, size: fmtBytes(r.freed) }) + (r.fail ? ' ' + t('audio.bulk_partial', { ok: r.ok, fail: r.fail }) : '') : t('audio.bulk_partial', { ok: 0, fail: r.fail }), r.fail ? 'err' : undefined)
  }

  void load()
  return () => { alive = false }
}
