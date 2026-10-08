import { api, pickFolder, toError, type ClientInfo, type DeleteMode, type Deletion } from './api'
import { t } from './i18n'
import { hooks, store, libName } from './store'
import { esc, fmtBytes, toast } from './util'

const modal = () => document.getElementById('modal') as HTMLDialogElement

const btn = 'rounded-xl border border-white/10 bg-ink-800 px-4 py-2 text-sm text-zinc-200 hover:border-violet-400/50'
const btnPrimary = 'rounded-xl border border-violet-400/60 bg-violet-500/20 px-4 py-2 text-sm font-medium text-white hover:bg-violet-500/30'
export const inputCls = 'w-full rounded-xl border border-white/10 bg-ink-950 px-3 py-2 text-sm text-zinc-100 placeholder:text-zinc-600 focus:border-violet-400/60 focus:outline-none focus:ring-2 focus:ring-violet-400/20'
export const btnCls = { btn, btnPrimary }

/**
 * Abre um formulário no <dialog>. `submit` devolve o resultado (fecha) ou lança erro
 * (mostrado no rodapé, o diálogo fica aberto). Esc/Cancelar resolve `null`.
 */
export function form<T>(title: string, body: string, okLabel: string, submit: (f: HTMLFormElement) => Promise<T>, setup?: (f: HTMLFormElement) => void, cancelLabel?: string): Promise<T | null> {
  const d = modal()
  d.style.width = ''
  d.innerHTML = `<form method="dialog" class="p-6">
    <h2 class="text-lg font-semibold text-white">${esc(title)}</h2>
    <div class="mt-4 space-y-4">${body}</div>
    <p data-err class="mt-3 min-h-5 text-sm text-rose-300"></p>
    <div class="mt-2 flex justify-end gap-2">
      <button type="button" value="cancel" data-cancel class="${btn}">${esc(cancelLabel ?? t('common.cancel'))}</button>
      <button type="submit" class="${btnPrimary}">${esc(okLabel)}</button>
    </div></form>`
  const f = d.querySelector('form')!
  setup?.(f)
  return new Promise(resolve => {
    let done = false
    const finish = (v: T | null) => { if (done) return; done = true; d.close(); d.removeEventListener('close', onClose); resolve(v) }
    f.querySelector('[data-cancel]')!.addEventListener('click', () => finish(null))
    // diálogos em sequência (pré-visualizar → confirmar): o `close` do anterior chega depois que o próximo
    // já abriu (d.open) e não pode fechá-lo
    const onClose = () => { if (!d.open) finish(null) }
    d.addEventListener('close', onClose)
    f.addEventListener('submit', async e => {
      e.preventDefault()
      const err = f.querySelector<HTMLElement>('[data-err]')!
      err.textContent = ''
      try { finish(await submit(f)) } catch (x) { err.textContent = describeError(x) }
    })
    d.showModal()
    f.querySelector<HTMLElement>('input, select, textarea')?.focus()
  })
}

export function describeError(x: unknown) {
  const e = toError(x)
  const known = t(`error.${e.code}`)
  return known === `error.${e.code}` ? e.detail : `${known}: ${e.detail}`
}

/** Erros do glossário: o `detail` do núcleo vem em inglês, então traduz pelo `code` (ctx opcional: `glossary.err.<ctx>_<code>`). */
export function describeGlossaryError(x: unknown, ctx = '') {
  const e = toError(x)
  for (const k of [ctx && `glossary.err.${ctx}_${e.code}`, `glossary.err.${e.code}`]) {
    if (k && t(k) !== k) return t(k)
  }
  return describeError(x)
}

export async function confirmDialog(title: string, message: string, okLabel: string, cancelLabel?: string): Promise<boolean> {
  const r = await form(title, `<p class="text-sm text-zinc-400">${esc(message)}</p>`, okLabel, async () => true, undefined, cancelLabel)
  return r === true
}

export const field = (label: string, input: string, hint = '') =>
  `<label class="block text-sm"><span class="mb-1.5 block text-zinc-400">${esc(label)}</span>${input}${hint ? `<span class="mt-1 block text-xs text-zinc-600">${hint}</span>` : ''}</label>`

export function addLibraryDialog() {
  return form(
    t('library.add_title'),
    field(t('library.name'), `<input name="name" required maxlength="80" class="${inputCls}" placeholder="${esc(t('library.name_ph'))}">`) +
      field(
        t('library.folder'),
        `<div class="flex gap-2"><input name="path" required class="${inputCls}" placeholder="/home/…"><button type="button" data-pick class="${btn} shrink-0">${esc(t('common.choose'))}</button></div>`,
        esc(t('library.folder_hint')),
      ),
    t('common.add'),
    async f => {
      const d = new FormData(f)
      return api.addLibrary(String(d.get('name')), String(d.get('path')))
    },
    f => f.querySelector('[data-pick]')!.addEventListener('click', async () => {
      const p = await pickFolder(t('library.folder'))
      if (p) (f.elements.namedItem('path') as HTMLInputElement).value = p
    }),
  )
}

export function addClientDialog(libraryId: number): Promise<ClientInfo | null> {
  const lib = store.libraries.find(l => l.id === libraryId)
  return form(
    t('client.add_title', { library: lib ? libName(lib) : '' }),
    field(t('client.name'), `<input name="name" required maxlength="80" class="${inputCls}">`),
    t('common.add'),
    async f => api.addClient(libraryId, String(new FormData(f).get('name'))),
  )
}

export function renameDialog(title: string, current: string, label: string, allowEmpty = false, hint = '') {
  return form(
    title,
    field(label, `<input name="v" ${allowEmpty ? '' : 'required'} maxlength="200" class="${inputCls}" value="${esc(current)}">`, hint),
    t('common.save'),
    async f => String(new FormData(f).get('v')).trim(),
  )
}

/** Escolhe empresa/projeto + cliente (ou "Não classificadas"). Devolve o destino. */
export function assignDialog(current: { library_id: number; client_id: number | null }) {
  const libs = store.libraries.filter(l => l.available)
  const opts = libs
    .map(l => `<option value="${l.id}" ${l.id === current.library_id ? 'selected' : ''}>${esc(libName(l))}</option>`)
    .join('')
  return form(
    t('assign.title'),
    field(t('assign.company'), `<select name="lib" class="${inputCls}">${opts}</select>`) +
      `<div data-client-box>${field(t('assign.client'), `<select name="client" class="${inputCls}"></select>`)}</div>` +
      field(t('assign.new_client'), `<input name="new_client" maxlength="80" class="${inputCls}" placeholder="${esc(t('assign.new_client_ph'))}">`),
    t('assign.ok'),
    async f => {
      const d = new FormData(f)
      const libraryId = Number(d.get('lib'))
      const lib = libs.find(l => l.id === libraryId)!
      let clientId: number | null = d.get('client') ? Number(d.get('client')) : null
      const newName = String(d.get('new_client') ?? '').trim()
      if (newName && lib.kind === 'company') clientId = (await api.addClient(libraryId, newName)).id
      return { libraryId, clientId }
    },
    f => {
      const libSel = f.elements.namedItem('lib') as HTMLSelectElement
      const fill = async () => {
        const lib = libs.find(l => l.id === Number(libSel.value))!
        const box = f.querySelector<HTMLElement>('[data-client-box]')!
        const newBox = (f.elements.namedItem('new_client') as HTMLInputElement).closest('label')!
        const isCompany = lib.kind === 'company'
        box.hidden = newBox.hidden = !isCompany
        const sel = f.elements.namedItem('client') as HTMLSelectElement
        const list = isCompany ? await api.clients(lib.id) : []
        sel.innerHTML = `<option value="">${esc(t('assign.no_client'))}</option>` +
          list.map(c => `<option value="${c.id}" ${c.id === current.client_id && lib.id === current.library_id ? 'selected' : ''}>${esc(c.name)}</option>`).join('')
      }
      libSel.addEventListener('change', fill)
      fill()
    },
  )
}

// ---- apagar empresa/cliente (o núcleo recusa antes de mexer em qualquer coisa; aqui é só o fluxo de confirmação)

const btnDanger = 'rounded-xl border border-rose-400/60 bg-rose-500/20 px-4 py-2 text-sm font-medium text-rose-100 hover:bg-rose-500/30'

export interface DeleteTarget { libraryId: number; clientId?: number; name: string }

/** Diálogo com várias saídas: cada escolha roda `run` com o diálogo aberto (botões travados e "Apagando…"); erro aparece no rodapé. */
function choiceDialog<T>(title: string, body: string, choices: { value: T; label: string; cls: string; disabled?: boolean }[], run: (v: T) => Promise<void>): Promise<boolean> {
  const d = modal()
  d.style.width = ''
  d.innerHTML = `<div class="p-6">
    <h2 class="text-lg font-semibold text-white">${esc(title)}</h2>
    <div class="mt-4 space-y-3">${body}</div>
    <p data-err class="mt-3 min-h-5 text-sm text-rose-300"></p>
    <div class="mt-2 flex flex-col gap-2">
      ${choices.map((c, i) => `<button type="button" data-choice="${i}" ${c.disabled ? 'disabled' : ''} class="${c.cls} text-left disabled:cursor-not-allowed disabled:opacity-40">${esc(c.label)}</button>`).join('')}
      <button type="button" data-cancel class="${btn}">${esc(t('common.cancel'))}</button>
    </div></div>`
  const all = () => [...d.querySelectorAll<HTMLButtonElement>('button')]
  return new Promise(resolve => {
    let busy = false, done = false
    const finish = (ok: boolean) => { if (done) return; done = true; d.removeEventListener('close', onClose); d.removeEventListener('cancel', onCancel); if (d.open) d.close(); resolve(ok) }
    const onClose = () => { if (!busy) finish(false) }
    const onCancel = (e: Event) => { if (busy) e.preventDefault() } // Esc não fecha no meio da operação
    d.addEventListener('close', onClose)
    d.addEventListener('cancel', onCancel)
    d.querySelector('[data-cancel]')!.addEventListener('click', () => { if (!busy) finish(false) })
    d.querySelectorAll<HTMLButtonElement>('[data-choice]').forEach(b => b.addEventListener('click', async () => {
      if (busy) return
      const i = Number(b.dataset.choice)
      const err = d.querySelector<HTMLElement>('[data-err]')!
      err.textContent = ''
      busy = true
      const was = all().map(x => x.disabled)
      all().forEach(x => { x.disabled = true })
      b.textContent = t('purge.working')
      try { await run(choices[i].value); busy = false; finish(true) }
      catch (x) { busy = false; err.textContent = describeError(x); all().forEach((e, j) => { e.disabled = was[j] }); b.textContent = choices[i].label }
    }))
    d.showModal()
  })
}

/** Depois de apagar: recarrega a navegação, sai da tela que mostrava o que sumiu e avisa o resultado. */
async function afterDeletion(target: DeleteTarget, r: Deletion) {
  const parts = (location.hash.slice(1) || '/').split('?')[0].split('/').filter(Boolean)
  const lib = Number(parts[1])
  let gone = false
  if (parts[0] === 'lib' && lib === target.libraryId) gone = target.clientId == null || (parts[2] === 'client' && Number(parts[3]) === target.clientId)
  else if (parts[0] === 'call' && lib === target.libraryId) {
    if (target.clientId == null) gone = true // a empresa sumiu (as chamadas foram para Não classificadas ou apagadas)
    else if (r.deleted) gone = await api.callDetail(lib, Number(parts[2])).then(() => false, () => true)
  }
  await hooks.reloadNav()
  if (gone) location.hash = '#/'
  const kind = target.clientId == null ? 'company' : 'client'
  const vars = { name: target.name, n: r.moved || r.deleted, count: r.moved || r.deleted }
  let msg = r.moved ? t(`purge.done_moved_${kind}`, vars) : r.deleted ? t(`purge.done_erased_${kind}`, vars) : t(`purge.done_${kind}`, vars)
  if (r.leftover.length) msg += ' ' + t('purge.leftover', { items: r.leftover.join(', ') })
  toast(msg)
}

/**
 * Apagar de verdade uma empresa/projeto ou um cliente. Simula antes (`dry_run`); sem chamadas é uma confirmação simples,
 * com chamadas o usuário escolhe mover (para Não classificadas / Sem cliente) ou apagar tudo (sem volta). Devolve `true` se apagou.
 */
export async function deleteDialog(target: DeleteTarget): Promise<boolean> {
  const isClient = target.clientId != null
  const kind = isClient ? 'client' : 'company'
  const run = (mode: DeleteMode | null, dryRun: boolean) => isClient
    ? api.deleteClient(target.libraryId, target.clientId!, mode, dryRun)
    : api.deleteLibrary(target.libraryId, mode, dryRun)
  let plan: Deletion
  try { plan = await run(null, true) } catch (e) { toast(describeError(e), 'err'); return false }
  const vars = { name: target.name }
  const note = (isClient ? plan.glossary_entries > 0 : plan.clients + plan.glossary_entries > 0)
    ? `<p class="text-xs text-zinc-500">${esc(t(`purge.${kind}_scope`, { clients: plan.clients, entries: plan.glossary_entries }))}</p>` : ''
  const exec = async (mode: DeleteMode | null) => { await afterDeletion(target, await run(mode, false)) }
  if (plan.calls === 0) {
    const ok = await form(
      t(`purge.${kind}_title`, vars),
      `<p class="text-sm text-zinc-400">${esc(t(`purge.${kind}_empty`, vars))}</p>${note}`,
      t('purge.confirm'),
      async f => {
        const bs = [...f.querySelectorAll<HTMLButtonElement>('button')]
        bs.forEach(b => { b.disabled = true })
        try { await exec(null); return true } catch (e) { bs.forEach(b => { b.disabled = false }); throw e }
      },
      f => { f.querySelector<HTMLElement>('[type=submit]')!.className = btnDanger },
    )
    return ok === true
  }
  const n = plan.calls
  const count = (k: string) => t(k, { n, count: n })
  const body = `<p class="text-sm text-zinc-300">${esc(t(`purge.${kind}_calls`, { name: target.name, calls: t('settings.calls', { n, count: n }), size: fmtBytes(plan.audio_bytes) }))}</p>${note}
    <p class="text-xs text-zinc-500">${esc(t(`purge.${kind}_keep_hint`))}</p>
    ${plan.keep_blocked ? `<p class="rounded-lg bg-amber-400/[0.06] px-3 py-2 text-xs text-amber-200">${esc(t('purge.keep_blocked', { reason: plan.keep_blocked }))}</p>` : ''}
    <p class="rounded-lg bg-rose-400/[0.06] px-3 py-2 text-xs text-rose-200">${esc(t('purge.erase_warn'))}</p>`
  return choiceDialog<DeleteMode>(
    t(`purge.${kind}_title`, vars),
    body,
    [
      { value: 'keep', label: count(`purge.keep_${kind}`), cls: btnPrimary, disabled: !!plan.keep_blocked },
      { value: 'delete', label: count('purge.erase'), cls: btnDanger },
    ],
    exec,
  )
}
