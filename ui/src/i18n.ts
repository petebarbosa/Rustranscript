// Traduções da interface. en-US é a referência completa: chave ausente em outro idioma cai no
// inglês. Plural: chaves `x_one` / `x_other`, escolhidas por Intl.PluralRules quando há `n`.
import ptBR from './locales/pt-BR.json'
import enUS from './locales/en-US.json'
import es419 from './locales/es-419.json'

type Dict = Record<string, string>
export const FALLBACK = 'en-US'
export const LANGS = { 'pt-BR': ptBR as Dict, 'en-US': enUS as Dict, 'es-419': es419 as Dict }
export type Lang = keyof typeof LANGS

let current: Lang = 'pt-BR'
let rules = new Intl.PluralRules(current)

export function resolveLang(tag: string | undefined | null): Lang {
  const t = (tag ?? '').toLowerCase()
  if (t.startsWith('pt')) return 'pt-BR'
  if (t.startsWith('es')) return 'es-419'
  return 'en-US'
}

export function setLang(l: Lang) {
  current = l
  rules = new Intl.PluralRules(l)
  document.documentElement.lang = l
}

export const lang = () => current

export function t(key: string, vars: Record<string, string | number> = {}): string {
  const dict = LANGS[current]
  const base = LANGS[FALLBACK]
  let k = key
  if (typeof vars.n === 'number') {
    // 0 conta como plural em todos os idiomas daqui (Intl devolve "one" para 0 em pt-BR)
    const plural = `${key}_${vars.n === 0 ? 'other' : rules.select(vars.n)}`
    const other = `${key}_other`
    // sem variantes de plural a chave simples vale (ex.: import.progress_call usa {n} só como número)
    k = plural in dict || plural in base ? plural : other in dict || other in base ? other : key
  }
  const s = dict[k] ?? base[k] ?? key
  return s.replace(/\{(\w+)\}/g, (_, v) => (v in vars ? String(vars[v]) : `{${v}}`))
}
