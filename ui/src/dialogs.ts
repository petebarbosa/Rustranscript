import { api, pickFolder, toError, type ClientInfo } from './api'
import { t } from './i18n'
import { store, libName } from './store'
import { esc } from './util'

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
