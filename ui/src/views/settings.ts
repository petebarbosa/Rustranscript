import { api, type DeviceInfo, type RecordDevices, type RecordInfo, type ShortcutInfo, type StreamChoice } from '../api'
import { LANGS, lang, setLang, t, type Lang } from '../i18n'
import { hooks, store, type View } from '../store'
import { addLibraryDialog, btnCls, confirmDialog, describeError, inputCls, renameDialog } from '../dialogs'
import { esc, fmtNumber, toast } from '../util'
import { mountTranscriptionSettings } from './txsettings'

// Trechos do compositor (RECORDING_CONTRACT §10.5, verificados em out/2026). A regra casa pelo título INICIAL da janela.
const HYPR_CONF = `windowrule = match:title ^(transcricoes-bar)$, float on
windowrule = match:title ^(transcricoes-bar)$, pin on
windowrule = match:title ^(transcricoes-bar)$, no_initial_focus on
windowrule = match:title ^(transcricoes-bar)$, move (monitor_w-window_w-20) 20
bind = CTRL ALT, R, exec, transcricoes record toggle`
const HYPR_LUA = `hl.window_rule({
  name = "transcricoes-bar",
  match = { title = "^(transcricoes-bar)$" },
  float = true,
  pin = true,
  no_initial_focus = true,
  move = { "monitor_w-window_w-20", "20" },
})
hl.bind("CTRL + ALT + R", hl.dsp.exec_cmd("transcricoes record toggle"))`

const valueChoice = (v: string): StreamChoice => (v === 'default' || v === 'off' ? v : { named: v.slice(4) })

/** Atalho a partir de um keydown no formato do plugin (Ctrl+Alt+R). Exige modificador; só modificador não vale. */
function accelFrom(e: KeyboardEvent): string | null {
  if (['Control', 'Alt', 'Shift', 'Meta', 'AltGraph'].includes(e.key)) return null
  const mods = [e.ctrlKey && 'Ctrl', e.altKey && 'Alt', e.shiftKey && 'Shift', e.metaKey && 'Super'].filter(Boolean) as string[]
  if (!mods.length) return null
  const key = e.code.startsWith('Key') ? e.code.slice(3) : e.code.startsWith('Digit') ? e.code.slice(5) : e.code
  return [...mods, key].join('+')
}

async function copyText(text: string) {
  try { await navigator.clipboard.writeText(text) }
  catch {
    const ta = document.createElement('textarea')
    ta.value = text; document.body.appendChild(ta); ta.select()
    try { document.execCommand('copy') } finally { ta.remove() }
  }
}

const LANG_NAMES: Record<Lang, string> = { 'pt-BR': 'Português (Brasil)', 'en-US': 'English (US)', 'es-419': 'Español (Latinoamérica)' }

export async function renderSettings(el: HTMLElement): Promise<View> {
  // gravação: se o shell não responde (ex.: comando ainda não pronto), a seção some e o resto da tela segue
  let info: RecordInfo | null = null
  let devs: RecordDevices | null = null
  try { [info, devs] = await Promise.all([api.recordInfo(), api.recordDevices()]) } catch { /* sem gravação */ }
  let shortcutMsg = ''

  const recSection = (): string => {
    if (!info) return ''
    const inf = info
    const dev = (list: DeviceInfo[], cur: StreamChoice) => {
      const known = typeof cur === 'string' || list.some(d => d.name === cur.named)
      return `<option value="default">${esc(t('record.dev_default'))}</option><option value="off">${esc(t('record.dev_off'))}</option>` +
        list.map(d => `<option value="dev:${esc(d.name)}">${esc(d.description || d.name)}${d.is_default ? ' · ' + esc(t('record.dev_is_default')) : ''}</option>`).join('') +
        (known ? '' : `<option value="dev:${esc((cur as { named: string }).named)}">${esc((cur as { named: string }).named)} · ${esc(t('record.dev_missing'))}</option>`)
    }
    const all = devs?.devices ?? []
    const sc = inf.shortcut
    const supported = sc.supported
    const wayland = inf.session_type !== 'x11'
    const hypr = wayland && (inf.desktop ?? '').includes('Hyprland')
    const block = (id: string, title: string, code: string) => `<div class="mt-4">
      <div class="flex items-center justify-between gap-3"><p class="text-xs font-medium text-zinc-300">${esc(title)}</p>
        <button type="button" data-copy="${id}" class="rounded-lg border border-white/10 bg-ink-800 px-2.5 py-1 text-xs text-zinc-300 hover:border-violet-400/50">${esc(t('settings.record.copy'))}</button></div>
      <pre id="snip-${id}" class="mt-2 overflow-x-auto rounded-xl bg-ink-950 p-3 text-xs text-zinc-300">${esc(code)}</pre></div>`
    return `<section id="rec-settings" class="mt-8 space-y-5 rounded-2xl border border-white/10 bg-ink-900/60 p-5">
      <h2 class="text-sm font-semibold text-white">${esc(t('settings.record.title'))}</h2>
      <div class="grid gap-4 sm:grid-cols-2">
        <label class="block text-sm"><span class="mb-1.5 block text-zinc-400">${esc(t('settings.record.mic_default'))}</span>
          <select id="rec-mic" class="${inputCls}">${dev(all.filter(d => !d.is_monitor), inf.last_used.mic)}</select></label>
        <label class="block text-sm"><span class="mb-1.5 block text-zinc-400">${esc(t('settings.record.sys_default'))}</span>
          <select id="rec-sys" class="${inputCls}">${dev(all.filter(d => d.is_monitor), inf.last_used.sys)}</select></label>
      </div>
      <p class="-mt-2 text-xs text-zinc-600">${esc(t('settings.record.devices_hint'))}</p>
      <label class="flex items-center gap-2 text-sm text-zinc-300"><input id="rec-bar" type="checkbox" ${inf.bar_on_start ? 'checked' : ''} class="accent-violet-500"> ${esc(t('settings.record.bar_on_start'))}</label>

      <div class="text-sm"><span class="mb-1.5 block text-zinc-400">${esc(t('settings.record.shortcut'))}</span>
        <div class="flex flex-wrap items-center gap-2">
          <input id="rec-shortcut" readonly ${supported ? '' : 'disabled'} value="${esc(sc.accelerator ?? '')}" placeholder="${esc(supported && sc.accelerator === null ? t('settings.record.shortcut_off') : '')}"
            class="${inputCls} !w-56 font-mono disabled:cursor-not-allowed disabled:opacity-50">
          <button id="rec-sc-off" type="button" ${supported ? '' : 'disabled'} class="${btnCls.btn} disabled:opacity-40">${esc(t('settings.record.shortcut_disable'))}</button>
          <button id="rec-sc-default" type="button" ${supported ? '' : 'disabled'} class="${btnCls.btn} disabled:opacity-40">${esc(t('settings.record.shortcut_default', { key: inf.shortcut_default }))}</button></div>
        <p id="rec-sc-msg" class="mt-1.5 min-h-4 text-xs ${shortcutMsg ? 'text-amber-300' : 'text-zinc-600'}">${esc(shortcutMsg || (supported ? t('settings.record.shortcut_hint') : ''))}</p>
        ${supported ? '' : `<p class="mt-1 rounded-xl border border-amber-400/20 bg-amber-400/[0.04] p-3 text-xs text-amber-200">${esc(t('settings.record.shortcut_unsupported', { session: inf.session_type }))}</p>`}
      </div>

      <div class="rounded-xl border border-amber-400/20 bg-amber-400/[0.04] p-4 text-xs text-zinc-400">
        <p class="font-semibold text-amber-200">${esc(t('record.warn_title'))}</p>
        <ul class="mt-2 list-disc space-y-1.5 pl-5"><li>${esc(t('record.warn_monitor'))}</li><li>${esc(t('record.warn_screen'))}</li><li>${esc(t('record.warn_consent'))}</li></ul></div>

      ${wayland && !hypr ? `<p class="text-xs text-zinc-500">${esc(t('settings.record.wayland_other'))} <code class="rounded bg-ink-950 px-1.5 py-0.5 text-zinc-300">transcricoes record toggle</code></p>` : ''}
      <details id="rec-hypr" ${hypr ? 'open' : ''} class="rounded-xl border border-white/10 bg-ink-950/50 p-4">
        <summary class="cursor-pointer text-sm font-medium text-zinc-200">${esc(t('settings.record.hypr_title'))}</summary>
        <p class="mt-3 text-xs text-zinc-500">${esc(t('settings.record.hypr_intro'))}</p>
        <p class="mt-2 rounded-lg bg-amber-400/[0.06] px-3 py-2 text-xs text-amber-200">${esc(t('settings.record.hypr_version'))}</p>
        ${block('conf', t('settings.record.hypr_conf'), HYPR_CONF)}
        ${block('lua', t('settings.record.hypr_lua'), HYPR_LUA)}
        <ul class="mt-4 list-disc space-y-1 pl-5 text-xs text-zinc-500"><li>${esc(t('settings.record.hypr_note_pin'))}</li><li>${esc(t('settings.record.hypr_note_title'))}</li><li>${esc(t('settings.record.hypr_note_bind'))}</li></ul>
      </details></section>`
  }

  let offTx: (() => void) | null = null
  const draw = () => {
    offTx?.()
    const companies = store.libraries.filter(l => l.kind === 'company')
    el.innerHTML = `<div class="mx-auto max-w-3xl px-6 py-10">
      <h1 class="text-3xl font-semibold tracking-tight text-white">${esc(t('settings.title'))}</h1>

      <section class="mt-8 space-y-5 rounded-2xl border border-white/10 bg-ink-900/60 p-5">
        <label class="block text-sm"><span class="mb-1.5 block text-zinc-400">${esc(t('settings.language'))}</span>
          <select id="lang" class="${inputCls}">${(Object.keys(LANGS) as Lang[]).map(l => `<option value="${l}" ${l === lang() ? 'selected' : ''}>${LANG_NAMES[l]}</option>`).join('')}</select></label>
        <label class="block text-sm"><span class="mb-1.5 block text-zinc-400">${esc(t('settings.me_name'))}</span>
          <input id="me" maxlength="80" class="${inputCls}" value="${esc(store.boot.settings.me_name ?? '')}" placeholder="${esc(t('speaker.me'))}">
          <span class="mt-1 block text-xs text-zinc-600">${esc(t('settings.me_hint'))}</span></label>
        <div class="text-sm"><span class="mb-1.5 block text-zinc-400">${esc(t('settings.data_dir'))}</span>
          <code class="block rounded-xl border border-white/10 bg-ink-950 px-3 py-2 text-xs text-zinc-300">${esc(store.boot.data_dir)}</code>
          <span class="mt-1 block text-xs text-zinc-600">${esc(t('settings.data_dir_hint'))}</span></div>
      </section>

      ${recSection()}

      <section id="tx-settings" class="mt-8"></section>

      <section class="mt-8">
        <div class="flex items-center justify-between">
          <h2 class="text-sm font-semibold text-white">${esc(t('nav.companies'))}</h2>
          <button id="add-lib" type="button" class="${btnCls.btn}">+ ${esc(t('nav.add_company'))}</button>
        </div>
        <p class="mt-1 text-xs text-zinc-600">${esc(t('settings.companies_hint'))}</p>
        <ul class="mt-3 space-y-2">${companies.map(l => `<li class="flex items-center gap-3 rounded-xl border border-white/10 bg-ink-900/60 px-4 py-3">
          <div class="min-w-0 flex-1"><p class="font-medium text-zinc-100">${esc(l.name)} ${l.available ? '' : `<span class="text-xs text-rose-300">· ${esc(t('nav.offline'))}</span>`}</p>
            <p class="truncate font-mono text-xs text-zinc-500">${esc(l.path)}</p></div>
          <span class="text-xs text-zinc-500">${esc(t('settings.calls', { n: l.call_count, count: fmtNumber(l.call_count) }))}</span>
          <button type="button" data-rename="${l.id}" class="rounded-lg px-2 py-1 text-xs text-zinc-400 hover:bg-white/5 hover:text-zinc-100">${esc(t('common.rename'))}</button>
          <button type="button" data-remove="${l.id}" class="rounded-lg px-2 py-1 text-xs text-zinc-400 hover:bg-rose-400/10 hover:text-rose-200">${esc(t('settings.unregister'))}</button>
        </li>`).join('') || `<li class="text-sm text-zinc-600">${esc(t('nav.no_companies'))}</li>`}</ul>
      </section>

      <section class="mt-8 rounded-2xl border border-white/10 bg-ink-900/60 p-5 text-sm text-zinc-400">
        <h2 class="font-semibold text-white">${esc(t('settings.cli_title'))}</h2>
        <p class="mt-1">${esc(t('settings.cli_hint'))}</p>
        <pre class="mt-3 overflow-x-auto rounded-xl bg-ink-950 p-3 text-xs text-zinc-300">transcricoes list
transcricoes search "gateway service"
transcricoes show call_AAAA-MM-DD_HH-MM-SS --text
transcricoes edit block call_… 12 "${esc(t('settings.cli_fixed_text'))}" --dry-run
transcricoes undo call_…
transcricoes --help</pre>
      </section></div>`

    el.querySelector('#lang')!.addEventListener('change', async e => {
      const l = (e.target as HTMLSelectElement).value as Lang
      await api.setSetting('language', l)
      store.boot.settings.language = l
      setLang(l)
      location.reload()
    })
    const me = el.querySelector<HTMLInputElement>('#me')!
    me.addEventListener('change', async () => {
      const v = me.value.trim()
      await api.setSetting('me_name', v || null)
      if (v) store.boot.settings.me_name = v
      else delete store.boot.settings.me_name
      toast(t('call.saved'))
    })
    bindRecord()
    offTx = mountTranscriptionSettings(el.querySelector<HTMLElement>('#tx-settings')!)
    el.querySelector('#add-lib')!.addEventListener('click', async () => {
      if (await addLibraryDialog()) { await hooks.reloadNav(); draw() }
    })
    el.querySelectorAll<HTMLElement>('[data-rename]').forEach(b => b.addEventListener('click', async () => {
      const l = store.libraries.find(x => x.id === Number(b.dataset.rename))!
      const v = await renameDialog(t('common.rename'), l.name, t('library.name'))
      if (!v) return
      try { await api.renameLibrary(l.id, v); await hooks.reloadNav(); draw() } catch (e) { toast(describeError(e), 'err') }
    }))
    el.querySelectorAll<HTMLElement>('[data-remove]').forEach(b => b.addEventListener('click', async () => {
      const l = store.libraries.find(x => x.id === Number(b.dataset.remove))!
      if (!(await confirmDialog(t('settings.unregister'), t('settings.unregister_confirm', { name: l.name, path: l.path }), t('settings.unregister')))) return
      try { await api.removeLibrary(l.id); await hooks.reloadNav(); draw() } catch (e) { toast(describeError(e), 'err') }
    }))
  }
  function bindRecord() {
    if (!info) return
    const inf = info
    const saveChoice = (key: 'record_mic' | 'record_sys', sel: HTMLSelectElement) => sel.addEventListener('change', async () => {
      const c = valueChoice(sel.value)
      try { await api.setSetting(key, JSON.stringify(c)); inf.last_used = { ...inf.last_used, [key === 'record_mic' ? 'mic' : 'sys']: c }; toast(t('call.saved')) }
      catch (e) { toast(describeError(e), 'err') }
    })
    saveChoice('record_mic', el.querySelector<HTMLSelectElement>('#rec-mic')!)
    saveChoice('record_sys', el.querySelector<HTMLSelectElement>('#rec-sys')!)
    el.querySelector<HTMLInputElement>('#rec-bar')!.addEventListener('change', async e => {
      const on = (e.target as HTMLInputElement).checked
      try { await api.setSetting('record_bar_on_start', on ? '1' : '0'); inf.bar_on_start = on; toast(t('call.saved')) }
      catch (x) { toast(describeError(x), 'err') }
    })
    const setShortcut = async (acc: string | null) => {
      try {
        const r: ShortcutInfo = await api.recordSetShortcut(acc)
        inf.shortcut = r
        shortcutMsg = r.error ? t('settings.record.shortcut_error', { error: r.error }) : ''
        if (!r.supported) shortcutMsg = ''
      } catch (e) { shortcutMsg = describeError(e) }
      draw()
    }
    const sc = el.querySelector<HTMLInputElement>('#rec-shortcut')!
    sc.addEventListener('keydown', e => {
      if (e.key === 'Tab') return
      e.preventDefault()
      if (e.key === 'Escape') { sc.blur(); return }
      const acc = accelFrom(e)
      if (acc) void setShortcut(acc)
      else shortcutMsg = ''
    })
    el.querySelector('#rec-sc-off')!.addEventListener('click', () => void setShortcut(null))
    el.querySelector('#rec-sc-default')!.addEventListener('click', () => void setShortcut(inf.shortcut_default))
    el.querySelectorAll<HTMLElement>('[data-copy]').forEach(b => b.addEventListener('click', async () => {
      await copyText(el.querySelector(`#snip-${b.dataset.copy}`)!.textContent ?? '')
      toast(t('settings.record.copied'))
    }))
  }

  draw()
  return { refresh: draw, dispose: () => offTx?.() }
}
