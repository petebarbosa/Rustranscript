import { lang, t } from './i18n'
import { SPEAKER_LABEL_ME, SPEAKER_LABEL_PERSON, type CallSummary, type SpeakerInfo } from './api'

export const esc = (s: string) =>
  s.replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]!)

export const rx = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')

/** Remove acentos e caixa, para busca local (o FTS do banco faz o mesmo). */
export const fold = (s: string) => s.normalize('NFD').replace(/\p{M}/gu, '').toLowerCase()

export function fmtTime(sec: number, long = false) {
  const s = Math.max(0, Math.floor(sec))
  const h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60), r = s % 60
  const mm = String(m).padStart(2, '0'), ss = String(r).padStart(2, '0')
  return h || long ? `${h}:${mm}:${ss}` : `${mm}:${ss}`
}

export function fmtDuration(sec: number) {
  const h = Math.floor(sec / 3600), m = Math.round((sec % 3600) / 60)
  return h ? t('duration.hm', { h, m }) : t('duration.m', { m: Math.max(1, m) })
}

const parseLocal = (iso: string) => new Date(iso.length === 10 ? iso + 'T00:00:00' : iso)

export const fmtDate = (iso: string) =>
  new Intl.DateTimeFormat(lang(), { day: 'numeric', month: 'long', year: 'numeric' }).format(parseLocal(iso))

export const fmtDateShort = (iso: string) =>
  new Intl.DateTimeFormat(lang(), { day: '2-digit', month: '2-digit' }).format(parseLocal(iso))

export const fmtClock = (iso: string) => iso.slice(11, 16)

export const fmtNumber = (n: number) => new Intl.NumberFormat(lang()).format(n)

export function fmtBytes(n: number) {
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  let i = 0
  while (n >= 1024 && i < units.length - 1) { n /= 1024; i++ }
  return `${new Intl.NumberFormat(lang(), { maximumFractionDigits: i > 1 ? 1 : 0 }).format(n)} ${units[i]}`
}

export function callTitle(c: Pick<CallSummary, 'title' | 'started_at'>) {
  return c.title || t('call.untitled', { date: fmtDateShort(c.started_at), time: fmtClock(c.started_at) })
}

/** Rótulo sem o nome dado pelo usuário: 'Eu' → nome configurado ou t('speaker.me'); 'Pessoa N' → t('speaker.person'). */
export function speakerDefault(s: SpeakerInfo | undefined, meName: string | undefined) {
  if (!s) return '?'
  if (s.track === 'mic' || s.label === SPEAKER_LABEL_ME) return meName || t('speaker.me')
  const m = SPEAKER_LABEL_PERSON.exec(s.label)
  return m ? t('speaker.person', { n: m[1] }) : s.label
}

/** Nome exibido: o que o usuário deu vence tudo. */
export const speakerName = (s: SpeakerInfo | undefined, meName: string | undefined) => s?.name || speakerDefault(s, meName)

/** Trecho do FTS (\u0002…\u0003) → HTML com <mark>. */
export const markSnippet = (s: string) =>
  esc(s).replace(/\u0002/g, '<mark>').replace(/\u0003/g, '</mark>')

export function h<K extends keyof HTMLElementTagNameMap>(tag: K, attrs: Record<string, string> = {}, html = '') {
  const el = document.createElement(tag)
  for (const [k, v] of Object.entries(attrs)) el.setAttribute(k, v)
  el.innerHTML = html
  return el
}

export function toast(msg: string, kind: 'ok' | 'err' = 'ok') {
  const box = document.getElementById('toast')!
  const color = kind === 'err' ? 'border-rose-400/40 text-rose-200' : 'border-violet-400/40 text-zinc-100'
  box.innerHTML = `<div class="rounded-xl border ${color} bg-ink-800/95 px-4 py-2 text-sm shadow-xl">${esc(msg)}</div>`
  clearTimeout((box as any)._t)
  ;(box as any)._t = setTimeout(() => (box.innerHTML = ''), kind === 'err' ? 6000 : 2500)
}

/** Toast com um link (o contêiner ignora o mouse; o cartão reativa só para si). `href` é um hash interno. */
export function toastLink(msg: string, href: string, label: string, ms = 9000) {
  const box = document.getElementById('toast')!
  box.innerHTML = `<div class="pointer-events-auto flex items-center gap-3 rounded-xl border border-violet-400/40 bg-ink-800/95 px-4 py-2 text-sm text-zinc-100 shadow-xl">
    <span>${esc(msg)}</span><a href="${esc(href)}" class="whitespace-nowrap font-medium text-violet-300 hover:underline">${esc(label)} →</a></div>`
  clearTimeout((box as any)._t)
  ;(box as any)._t = setTimeout(() => (box.innerHTML = ''), ms)
}

export const debounce = <A extends unknown[]>(fn: (...a: A) => void, ms: number) => {
  let id: ReturnType<typeof setTimeout> | undefined
  return (...a: A) => { clearTimeout(id); id = setTimeout(() => fn(...a), ms) }
}

/**
 * Diferença por palavra (LCS) entre dois textos, já em HTML escapado: o que sai em `before`
 * vem em <del>, o que entra em `after` vem em <ins>. Textos de bloco têm centenas de palavras no máximo.
 */
export function diffWords(a: string, b: string): { before: string; after: string } {
  const x = a.split(/(\s+)/).filter(Boolean), y = b.split(/(\s+)/).filter(Boolean)
  const lcs: number[][] = Array.from({ length: x.length + 1 }, () => new Array(y.length + 1).fill(0))
  for (let i = x.length - 1; i >= 0; i--)
    for (let j = y.length - 1; j >= 0; j--)
      lcs[i][j] = x[i] === y[j] ? lcs[i + 1][j + 1] + 1 : Math.max(lcs[i + 1][j], lcs[i][j + 1])
  let before = '', after = '', i = 0, j = 0
  const del = (s: string) => (/^\s+$/.test(s) ? esc(s) : `<del class="rounded bg-rose-400/15 text-rose-200 no-underline">${esc(s)}</del>`)
  const ins = (s: string) => (/^\s+$/.test(s) ? esc(s) : `<ins class="rounded bg-emerald-400/15 text-emerald-200 no-underline">${esc(s)}</ins>`)
  while (i < x.length || j < y.length) {
    if (i < x.length && j < y.length && x[i] === y[j]) { before += esc(x[i]); after += esc(y[j]); i++; j++ }
    else if (j >= y.length || (i < x.length && lcs[i + 1][j] >= lcs[i][j + 1])) before += del(x[i++])
    else after += ins(y[j++])
  }
  return { before, after }
}

/** Barra de progresso; `fraction` null = indeterminada (animação). */
export function barHtml(fraction: number | null, tone = 'bg-violet-400') {
  const track = 'h-1.5 overflow-hidden rounded-full bg-white/10'
  if (fraction == null) return `<div class="${track}" role="progressbar"><div class="indet h-full w-1/3 rounded-full ${tone}"></div></div>`
  const pct = Math.round(Math.min(1, Math.max(0, fraction)) * 100)
  return `<div class="${track}" role="progressbar" aria-valuenow="${pct}" aria-valuemin="0" aria-valuemax="100"><div class="h-full rounded-full ${tone} transition-[width] duration-700 ease-out" style="width:${pct}%"></div></div>`
}
