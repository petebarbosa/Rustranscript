// Peças das regras de glossário compartilhadas entre a tela Glossário e o modal da chamada.
import type { Rule, RuleKind } from './api'
import { t } from './i18n'
import { field, inputCls } from './dialogs'
import { esc } from './util'

/** Erro de validação mostrado como os demais (mesmo formato que `describeGlossaryError` devolve). */
export const fail = (key: string): never => { throw { code: 'ui', detail: t(key) } }

/** Campos de uma regra (tipo, padrão, substituição, maiúsculas/minúsculas). */
export const ruleFields = (r: Partial<Rule>, kind: RuleKind) => `
  ${field(t('glossary.f_kind'), `<select name="kind" class="${inputCls}">
    <option value="replace" ${kind === 'replace' ? 'selected' : ''}>${esc(t('glossary.kind_replace'))}</option>
    <option value="term" ${kind === 'term' ? 'selected' : ''}>${esc(t('glossary.kind_term'))}</option></select>`)}
  ${field(t('glossary.f_pattern'), `<input name="pattern" required maxlength="200" class="${inputCls}" value="${esc(r.pattern ?? '')}" placeholder="${esc(t('glossary.f_pattern_ph'))}">`, esc(t('glossary.f_pattern_hint')))}
  <div data-rep>${field(t('glossary.f_replacement'), `<input name="replacement" maxlength="500" class="${inputCls}" value="${esc(r.replacement ?? '')}" placeholder="${esc(t('glossary.f_replacement_ph'))}">`)}</div>
  <label class="flex items-start gap-2 text-sm text-zinc-300"><input name="case" type="checkbox" ${r.case_sensitive ? 'checked' : ''} class="mt-0.5 accent-violet-500">
    <span>${esc(t('glossary.f_case'))}<span class="block text-xs text-zinc-600">${esc(t('glossary.f_case_hint'))}</span></span></label>`

/** Mostra/esconde o campo de substituição conforme o tipo. */
export const syncKind = (root: ParentNode) => {
  const sel = root.querySelector<HTMLSelectElement>('[name="kind"]')!
  const go = () => { root.querySelector<HTMLElement>('[data-rep]')!.hidden = sel.value === 'term' }
  sel.addEventListener('change', go)
  go()
}

/** Lê e valida os campos de `ruleFields` (um formulário ou qualquer contêiner com eles). */
export function readRule(root: ParentNode) {
  const get = (n: string) => root.querySelector<HTMLInputElement | HTMLSelectElement>(`[name="${n}"]`)
  const kind = (get('kind')?.value ?? 'replace') as RuleKind
  const pattern = String(get('pattern')?.value ?? '').replace(/\s+/g, ' ').trim()
  const replacement = kind === 'replace' ? String(get('replacement')?.value ?? '').replace(/\s+/g, ' ').trim() : null
  if (!pattern) fail('glossary.err.pattern_empty')
  if (kind === 'replace') {
    if (!replacement) fail('glossary.err.replacement_empty')
    if (replacement === pattern) fail('glossary.err.same')
  }
  return { kind, pattern, replacement, caseSensitive: !!(get('case') as HTMLInputElement | null)?.checked }
}

/** "padrão → substituição" (ou só o termo); `strike` risca as regras cobertas por outra. */
export function ruleText(r: Rule, strike = '') {
  return r.kind === 'replace'
    ? `<span class="text-rose-200/90 ${strike}">${esc(r.pattern)}</span> <span class="text-zinc-600">→</span> <span class="text-emerald-200 ${strike}">${esc(r.replacement ?? '')}</span>`
    : `<span class="text-zinc-100 ${strike}">${esc(r.pattern)}</span>`
}

export const caseBadge = (r: Rule) => r.case_sensitive
  ? `<span title="${esc(t('glossary.case_hint'))}" class="shrink-0 rounded-full border border-white/10 px-2 py-0.5 text-[11px] text-zinc-400">Aa · ${esc(t('glossary.case_short'))}</span>`
  : ''
