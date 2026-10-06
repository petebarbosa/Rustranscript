import { api, pickFile, type GlossaryImportReport, type PromptTerms, type Rule, type RuleKind, type Scope } from '../api'
import { t } from '../i18n'
import { libName, store, type View } from '../store'
import { btnCls, confirmDialog, describeGlossaryError, form, inputCls } from '../dialogs'
import { esc, fmtNumber, fold, toast } from '../util'

interface Ctx { libraryId: number | null; clientId: number | null }

// contexto e aba lembrados enquanto a app está aberta
let lastCtx: Ctx = { libraryId: null, clientId: null }
let lastTab: RuleKind = 'replace'

const fail = (key: string): never => { throw { code: 'ui', detail: t(key) } }
const field = (label: string, input: string, hint = '') =>
  `<label class="block text-sm"><span class="mb-1.5 block text-zinc-400">${esc(label)}</span>${input}${hint ? `<span class="mt-1 block text-xs text-zinc-600">${esc(hint)}</span>` : ''}</label>`

export async function renderGlossary(el: HTMLElement, params: URLSearchParams): Promise<View> {
  const companies = store.libraries.filter(l => l.kind === 'company' && l.available)
  const contexts: { value: string; label: string; ctx: Ctx }[] = [
    { value: 'global', label: t('glossary.ctx_global'), ctx: { libraryId: null, clientId: null } },
  ]
  for (const l of companies)
    for (const c of store.clients.get(l.id) ?? [])
      contexts.push({ value: `${l.id}:${c.id}`, label: `${libName(l)} · ${c.name}`, ctx: { libraryId: l.id, clientId: c.id } })
  const keyOf = (c: Ctx) => (c.libraryId == null ? 'global' : `${c.libraryId}:${c.clientId}`)

  let ctx = lastCtx
  if (params.get('lib') && params.get('client')) ctx = { libraryId: Number(params.get('lib')), clientId: Number(params.get('client')) }
  if (!contexts.some(c => c.value === keyOf(ctx))) ctx = contexts[0].ctx
  let tab: RuleKind = lastTab
  let rules: Rule[] = []
  let prompt: PromptTerms | null = null

  const isClient = () => ctx.libraryId != null
  const ctxLabel = () => contexts.find(c => c.value === keyOf(ctx))?.label ?? ''
  // as regras de cliente ficam na biblioteca do cliente; as globais vivem no app.db (qualquer biblioteca serve para o prompt)
  const promptLib = () => ctx.libraryId ?? store.boot.inbox_id

  async function load() {
    rules = await api.glossaryList(ctx.libraryId, ctx.clientId)
    prompt = await api.glossaryPromptTerms(promptLib(), ctx.clientId).catch(() => null)
  }

  // ------------------------------------------------------------------ tela
  function originBadge(r: Rule) {
    return r.scope === 'client'
      ? `<span class="shrink-0 rounded-full bg-emerald-400/10 px-2 py-0.5 text-[11px] font-medium text-emerald-300">${esc(t('glossary.origin_client'))}</span>`
      : `<span class="shrink-0 rounded-full bg-sky-400/10 px-2 py-0.5 text-[11px] font-medium text-sky-300">${esc(t('glossary.origin_global'))}</span>`
  }

  function rowHtml(r: Rule) {
    const dim = r.overridden ? 'opacity-50' : ''
    const strike = r.overridden ? 'line-through decoration-zinc-500' : ''
    const text = r.kind === 'replace'
      ? `<span class="text-rose-200/90 ${strike}">${esc(r.pattern)}</span> <span class="text-zinc-600">→</span> <span class="text-emerald-200 ${strike}">${esc(r.replacement ?? '')}</span>`
      : `<span class="text-zinc-100 ${strike}">${esc(r.pattern)}</span>`
    const act = 'rounded-lg px-2 py-1 text-xs text-zinc-400 hover:bg-white/5 hover:text-zinc-100'
    return `<li data-rule data-scope="${r.scope}" data-id="${r.id}" data-lib="${r.library_id ?? ''}" class="flex items-center gap-3 rounded-xl border border-white/10 bg-ink-900/60 px-4 py-3">
      <div class="flex min-w-0 flex-1 items-center gap-3 ${dim}">
        ${originBadge(r)}
        <div class="min-w-0 flex-1">
          <p class="break-words font-mono text-sm">${text}</p>
          ${r.overridden ? `<p class="mt-0.5 text-xs text-amber-300/80">${esc(t('glossary.overridden'))}</p>` : ''}
        </div>
        ${r.case_sensitive ? `<span title="${esc(t('glossary.case_hint'))}" class="shrink-0 rounded-full border border-white/10 px-2 py-0.5 text-[11px] text-zinc-400">Aa · ${esc(t('glossary.case_short'))}</span>` : ''}
      </div>
      <div class="flex shrink-0 items-center">
        <button type="button" data-edit class="${act}">${esc(t('common.edit'))}</button>
        ${r.scope === 'client' ? `<button type="button" data-promote title="${esc(t('glossary.promote_hint'))}" class="${act}">${esc(t('glossary.promote'))}</button>` : ''}
        <button type="button" data-remove class="rounded-lg px-2 py-1 text-xs text-zinc-400 hover:bg-rose-400/10 hover:text-rose-200">${esc(t('common.remove'))}</button>
      </div></li>`
  }

  function promptInfo() {
    if (!prompt) return ''
    // termos distintos em vigor (cliente primeiro; as globais cobertas não contam)
    const eff = new Set(rules.filter(r => r.kind === 'term' && !r.overridden).map(r => fold(r.pattern)))
    const total = eff.size, fit = prompt.terms.length
    if (!total) return ''
    const pct = Math.min(100, Math.round((prompt.estimated_tokens * 100) / Math.max(1, prompt.budget_tokens)))
    const over = fit < total
    return `<div class="mt-4 rounded-xl border ${over ? 'border-amber-400/30 bg-amber-400/5' : 'border-white/10 bg-ink-900/60'} px-4 py-3 text-sm">
      <div class="flex items-center justify-between gap-3">
        <span class="${over ? 'text-amber-200' : 'text-zinc-300'}">${esc(t('glossary.prompt_usage', { fit, total }))}</span>
        <span class="text-xs tabular-nums text-zinc-500">${esc(t('glossary.prompt_tokens', { used: prompt.estimated_tokens, budget: prompt.budget_tokens }))}</span>
      </div>
      <div class="mt-2 h-1.5 overflow-hidden rounded-full bg-white/5"><div class="h-full rounded-full ${over ? 'bg-amber-400' : 'bg-violet-500'}" style="width:${pct}%"></div></div>
      ${over ? `<p class="mt-2 text-xs text-amber-300/80">${esc(t('glossary.prompt_over'))}</p>` : ''}</div>`
  }

  function draw() {
    const shown = rules.filter(r => r.kind === tab)
    const count = (k: RuleKind) => rules.filter(r => r.kind === k && !r.overridden).length
    const tabBtn = (k: RuleKind, label: string) => `<button type="button" data-tab="${k}" role="tab" aria-selected="${tab === k}"
      class="rounded-lg px-3 py-1.5 text-sm ${tab === k ? 'bg-white/10 text-white' : 'text-zinc-400 hover:bg-white/5 hover:text-zinc-100'}">${esc(label)} <span class="text-xs tabular-nums text-zinc-500">${fmtNumber(count(k))}</span></button>`
    el.innerHTML = `<div class="mx-auto max-w-4xl px-6 py-10">
      <h1 class="text-3xl font-semibold tracking-tight text-white">${esc(t('glossary.title'))}</h1>
      <p class="mt-2 max-w-3xl text-sm text-zinc-500">${esc(t('glossary.intro'))}</p>

      <div class="mt-6 flex flex-wrap items-end gap-3">
        <label class="min-w-[16rem] basis-full flex-1 text-sm min-[900px]:basis-0"><span class="mb-1.5 block text-zinc-400">${esc(t('glossary.context'))}</span>
          <select id="ctx" class="${inputCls}">${contexts.map(c => `<option value="${esc(c.value)}" ${c.value === keyOf(ctx) ? 'selected' : ''}>${esc(c.label)}</option>`).join('')}</select></label>
        <button id="import" type="button" class="${btnCls.btn}">${esc(t('glossary.import'))}</button>
        <button id="add" type="button" class="${btnCls.btnPrimary}">+ ${esc(t(tab === 'replace' ? 'glossary.add_replace' : 'glossary.add_term'))}</button>
      </div>
      <p class="mt-2 text-xs text-zinc-600">${esc(t(isClient() ? 'glossary.ctx_client_hint' : 'glossary.ctx_global_hint'))}</p>

      <div role="tablist" class="mt-6 flex gap-1 border-b border-white/10 pb-2">
        ${tabBtn('replace', t('glossary.tab_replace'))}${tabBtn('term', t('glossary.tab_term'))}
      </div>
      <p class="mt-3 text-xs text-zinc-500">${esc(t(tab === 'replace' ? 'glossary.replace_hint' : 'glossary.term_hint'))}</p>
      ${tab === 'term' ? promptInfo() : ''}

      <ul id="rules" class="mt-4 space-y-2">${shown.map(rowHtml).join('') || `<li class="rounded-xl border border-dashed border-white/10 px-4 py-8 text-center text-sm text-zinc-600">${esc(t(tab === 'replace' ? 'glossary.empty_replace' : 'glossary.empty_term'))}</li>`}</ul>
    </div>`
    bind()
  }

  const rowRule = (li: HTMLElement) =>
    rules.find(r => r.scope === li.dataset.scope && r.id === Number(li.dataset.id) && (r.library_id ?? '') === (li.dataset.lib ? Number(li.dataset.lib) : ''))!

  async function reload() { await load(); draw() }

  // ------------------------------------------------------------------ ações
  const ruleFields = (r: Partial<Rule>, kind: RuleKind) => `
    ${field(t('glossary.f_kind'), `<select name="kind" class="${inputCls}">
      <option value="replace" ${kind === 'replace' ? 'selected' : ''}>${esc(t('glossary.kind_replace'))}</option>
      <option value="term" ${kind === 'term' ? 'selected' : ''}>${esc(t('glossary.kind_term'))}</option></select>`)}
    ${field(t('glossary.f_pattern'), `<input name="pattern" required maxlength="200" class="${inputCls}" value="${esc(r.pattern ?? '')}" placeholder="${esc(t('glossary.f_pattern_ph'))}">`, t('glossary.f_pattern_hint'))}
    <div data-rep>${field(t('glossary.f_replacement'), `<input name="replacement" maxlength="500" class="${inputCls}" value="${esc(r.replacement ?? '')}" placeholder="${esc(t('glossary.f_replacement_ph'))}">`)}</div>
    <label class="flex items-start gap-2 text-sm text-zinc-300"><input name="case" type="checkbox" ${r.case_sensitive ? 'checked' : ''} class="mt-0.5 accent-violet-500">
      <span>${esc(t('glossary.f_case'))}<span class="block text-xs text-zinc-600">${esc(t('glossary.f_case_hint'))}</span></span></label>`

  /** Mostra/esconde o campo de substituição conforme o tipo. */
  const syncKind = (f: HTMLFormElement) => {
    const sel = f.elements.namedItem('kind') as HTMLSelectElement
    const go = () => { f.querySelector<HTMLElement>('[data-rep]')!.hidden = sel.value === 'term' }
    sel.addEventListener('change', go)
    go()
  }

  function readRule(f: HTMLFormElement) {
    const d = new FormData(f)
    const kind = String(d.get('kind')) as RuleKind
    const pattern = String(d.get('pattern') ?? '').replace(/\s+/g, ' ').trim()
    const replacement = kind === 'replace' ? String(d.get('replacement') ?? '').replace(/\s+/g, ' ').trim() : null
    if (!pattern) fail('glossary.err.pattern_empty')
    if (kind === 'replace') {
      if (!replacement) fail('glossary.err.replacement_empty')
      if (replacement === pattern) fail('glossary.err.same')
    }
    return { kind, pattern, replacement, caseSensitive: d.get('case') === 'on' }
  }

  async function addRule() {
    const own = isClient()
    const scopeField = own
      ? field(t('glossary.f_scope'), `<select name="scope" class="${inputCls}">
          <option value="client">${esc(t('glossary.scope_client', { name: ctxLabel() }))}</option>
          <option value="global">${esc(t('glossary.scope_global'))}</option></select>`)
      : ''
    const r = await form(t('glossary.add_title'), scopeField + ruleFields({}, tab), t('common.add'), async f => {
      const input = readRule(f)
      const scope = ((f.elements.namedItem('scope') as HTMLSelectElement | null)?.value ?? 'global') as Scope
      try {
        return await api.glossaryAdd({ ...input, scope, libraryId: scope === 'client' ? ctx.libraryId : null, clientId: scope === 'client' ? ctx.clientId : null })
      } catch (e) { throw { code: 'ui', detail: describeGlossaryError(e) } }
    }, syncKind)
    if (!r) return
    tab = lastTab = r.kind
    toast(t('glossary.added'))
    await reload()
  }

  async function editRule(r: Rule) {
    const done = await form(t('glossary.edit_title'), ruleFields(r, r.kind), t('common.save'), async f => {
      const input = readRule(f)
      try { return await api.glossaryUpdate({ ...input, scope: r.scope, libraryId: r.library_id, id: r.id }) }
      catch (e) { throw { code: 'ui', detail: describeGlossaryError(e) } }
    }, syncKind)
    if (!done) return
    tab = lastTab = done.kind
    toast(t('call.saved'))
    await reload()
  }

  async function removeRule(r: Rule) {
    const what = r.kind === 'replace' ? `${r.pattern} → ${r.replacement}` : r.pattern
    if (!(await confirmDialog(t('glossary.remove_title'), t('glossary.remove_confirm', { what }), t('common.remove')))) return
    try { await api.glossaryRemove(r.scope, r.library_id, r.id); toast(t('glossary.removed')); await reload() }
    catch (e) { toast(describeGlossaryError(e), 'err') }
  }

  async function promoteRule(r: Rule) {
    if (!(await confirmDialog(t('glossary.promote_title'), t('glossary.promote_confirm', { pattern: r.pattern }), t('glossary.promote')))) return
    try { await api.glossaryPromote(r.library_id!, r.id); toast(t('glossary.promoted')); await reload() }
    catch (e) { toast(describeGlossaryError(e, 'promote'), 'err') }
  }

  // importar lista: escolhe o arquivo → simulação (o que entra e o que fica de fora) → confirma
  function importPreviewBody(rep: GlossaryImportReport) {
    const chip = (cls: string, label: string) => `<span class="rounded-full px-2 py-0.5 text-[11px] ${cls}">${esc(label)}</span>`
    const status = { added: chip('bg-emerald-400/10 text-emerald-300', t('glossary.imp_added')), duplicate: chip('bg-white/5 text-zinc-400', t('glossary.imp_duplicate')), invalid: chip('bg-rose-400/10 text-rose-300', t('glossary.imp_invalid')) }
    const reason = (e: GlossaryImportReport['entries'][number]) => {
      if (!e.reason) return ''
      const k = `glossary.imp_reason.${e.reason}`
      return t(k) === k ? e.reason : t(k)
    }
    const rows = rep.entries.map(e => `<li class="flex items-center gap-2 border-t border-white/5 py-1.5 first:border-0">
      <span class="w-8 shrink-0 text-right font-mono text-[11px] text-zinc-600">${e.line}</span>${status[e.status]}
      <span class="min-w-0 flex-1 break-words font-mono text-xs text-zinc-300">${esc(e.pattern)}${e.replacement != null ? ` <span class="text-zinc-600">→</span> ${esc(e.replacement)}` : ''}</span>
      ${e.reason ? `<span class="shrink-0 text-[11px] text-zinc-500">${esc(reason(e))}</span>` : ''}</li>`).join('')
    return `<p class="text-sm text-zinc-300">${esc(t('glossary.imp_summary', { added: rep.added, skipped: rep.skipped, invalid: rep.invalid }))}</p>
      <ul class="max-h-[45vh] overflow-y-auto rounded-xl border border-white/10 bg-ink-950/60 px-3 py-1">${rows || `<li class="py-3 text-sm text-zinc-500">${esc(t('glossary.imp_empty'))}</li>`}</ul>`
  }

  async function importList() {
    const target = isClient()
      ? field(t('glossary.f_scope'), `<select name="scope" class="${inputCls}">
          <option value="client">${esc(t('glossary.scope_client', { name: ctxLabel() }))}</option>
          <option value="global">${esc(t('glossary.scope_global'))}</option></select>`)
      : ''
    const first = await form(t('glossary.import_title'),
      field(t('glossary.imp_file'), `<div class="flex gap-2"><input name="path" required class="${inputCls}" placeholder="/home/…/vocabulary.txt"><button type="button" data-pick class="${btnCls.btn} shrink-0">${esc(t('common.choose'))}</button></div>`, t('glossary.imp_format')) +
      target +
      field(t('glossary.imp_kind'), `<select name="kind" class="${inputCls}">
        <option value="">${esc(t('glossary.imp_kind_all'))}</option>
        <option value="replace">${esc(t('glossary.imp_kind_replace'))}</option>
        <option value="term">${esc(t('glossary.imp_kind_term'))}</option></select>`),
      t('glossary.imp_preview'),
      async f => {
        const d = new FormData(f)
        const args = {
          path: String(d.get('path')).trim(),
          scope: (String(d.get('scope') ?? 'global') || 'global') as Scope,
          libraryId: ctx.libraryId, clientId: ctx.clientId,
          kind: (String(d.get('kind') ?? '') || null) as RuleKind | null,
        }
        if (args.scope === 'global') { args.libraryId = null; args.clientId = null }
        try { return { args, rep: await api.glossaryImportFile({ ...args, dryRun: true }) } }
        catch (e) { throw { code: 'ui', detail: describeGlossaryError(e, 'import') } }
      },
      f => f.querySelector('[data-pick]')!.addEventListener('click', async () => {
        const p = await pickFile(t('glossary.imp_file'))
        if (p) (f.elements.namedItem('path') as HTMLInputElement).value = p
      }))
    if (!first) return
    const { args, rep } = first
    if (!rep.added) {
      // nada a importar: só informa (sem segundo passo)
      await form(t('glossary.import_title'), importPreviewBody(rep), t('common.close'), async () => true, f => (f.closest('dialog')!.style.width = 'min(40rem,calc(100vw - 2rem))'))
      return
    }
    const done = await form(t('glossary.import_title'), importPreviewBody(rep), t('glossary.imp_confirm', { n: rep.added }),
      async () => {
        try { return await api.glossaryImportFile({ ...args, dryRun: false }) }
        catch (e) { throw { code: 'ui', detail: describeGlossaryError(e, 'import') } }
      }, f => (f.closest('dialog')!.style.width = 'min(40rem,calc(100vw - 2rem))'))
    if (!done) return
    toast(t('glossary.imp_done', { added: done.added, skipped: done.skipped, invalid: done.invalid }))
    await reload()
  }

  function bind() {
    el.querySelector<HTMLSelectElement>('#ctx')!.addEventListener('change', async e => {
      ctx = lastCtx = contexts.find(c => c.value === (e.target as HTMLSelectElement).value)!.ctx
      await reload()
    })
    el.querySelectorAll<HTMLElement>('[data-tab]').forEach(b => b.addEventListener('click', () => { tab = lastTab = b.dataset.tab as RuleKind; draw() }))
    el.querySelector('#add')!.addEventListener('click', addRule)
    el.querySelector('#import')!.addEventListener('click', importList)
    el.querySelector('#rules')!.addEventListener('click', e => {
      const btn = (e.target as HTMLElement).closest<HTMLElement>('button')
      const li = btn?.closest<HTMLElement>('[data-rule]')
      if (!btn || !li) return
      const r = rowRule(li)
      if (btn.hasAttribute('data-edit')) editRule(r)
      else if (btn.hasAttribute('data-remove')) removeRule(r)
      else if (btn.hasAttribute('data-promote')) promoteRule(r)
    })
  }

  await load()
  draw()
  return {
    refresh: reload,
    busy: () => !!document.querySelector('dialog[open]'),
  }
}
