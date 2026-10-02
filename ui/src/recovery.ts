// Diálogos globais da janela principal ligados à gravação: recuperação de gravações órfãs (queda do app/sistema ou
// finalização interrompida) e o toast de fim de finalização.
// O diálogo tem o SEU <dialog>: o `#modal` compartilhado resolve qualquer promessa aberta ao fechar, e uma confirmação
// de "Descartar" aninhada derrubaria a lista. A confirmação do descarte é feita na própria linha.
// (Fechar com gravação ativa não passa pela UI: o shell esconde a janela, ou pergunta por diálogo nativo no "Sair" do tray.)
import { api, on, REC_EVENTS, type FinalizeDone, type Orphan } from './api'
import { t } from './i18n'
import { hub, progressFrac, stageLabel, startHub, subscribe } from './rec'
import { libName, store } from './store'
import { btnCls, describeError } from './dialogs'
import { callTitle, esc, fmtBytes, fmtClock, fmtDate, fmtTime, toast, toastLink } from './util'

const dialogCls = 'm-auto w-[min(40rem,calc(100vw-2rem))] rounded-2xl border border-white/10 bg-ink-900 p-0 text-zinc-200 shadow-2xl backdrop:bg-black/60'
function ownDialog(id: string): HTMLDialogElement {
  let d = document.getElementById(id) as HTMLDialogElement | null
  if (!d) {
    d = document.createElement('dialog')
    d.id = id
    d.className = dialogCls
    document.body.appendChild(d)
  }
  return d
}

// ---------------------------------------------------------------- recuperação
let orphans: Orphan[] = []
const seen = new Set<string>() // chaves já apresentadas: o boot (comando) e o evento trazem a mesma lista
const rows = new Map<string, { confirming?: boolean; busy?: boolean; error?: string }>()

function place(o: Orphan): string {
  const i = o.intent
  if (!i) return t('nav.unclassified')
  const lib = store.libraries.find(l => l.id === i.library_id)
  const client = store.clients.get(i.library_id)?.find(c => c.id === i.client_id)
  return [lib ? libName(lib) : '', client?.name].filter(Boolean).join(' · ')
}

function orphanRow(o: Orphan): string {
  const st = rows.get(o.key) ?? {}
  const p = hub.progress.get(o.key)
  const title = callTitle({ title: o.intent?.title ?? '', started_at: o.started_at })
  const devices = [o.mic_device && `${t('record.mic')}: ${o.mic_device}`, o.sys_device && `${t('record.sys')}: ${o.sys_device}`].filter(Boolean).join(' · ')
  const chip = (s: string, cls = 'bg-white/[0.04] text-zinc-400') => `<span class="shrink-0 rounded-full px-2 py-0.5 text-xs ${cls}">${esc(s)}</span>`
  const actions = st.busy
    ? `<div class="mt-3"><div class="flex justify-between text-xs text-zinc-500"><span>${esc(p ? stageLabel(p) : t('recovery.working'))}</span></div>
        <div class="mt-1.5 h-2 overflow-hidden rounded-full bg-white/5"><div class="h-full rounded-full bg-violet-500 transition-[width]" style="width:${Math.round((p ? progressFrac(p) : 0.03) * 100)}%"></div></div></div>`
    : st.confirming
      ? `<div class="mt-3 flex flex-wrap items-center justify-end gap-2"><span class="mr-auto text-xs text-rose-200">${esc(t('recovery.discard_confirm'))}</span>
          <button type="button" data-act="cancel-discard" data-key="${esc(o.key)}" class="${btnCls.btn} !px-3 !py-1.5">${esc(t('common.cancel'))}</button>
          <button type="button" data-act="discard-yes" data-key="${esc(o.key)}" class="rounded-xl border border-rose-400/60 bg-rose-500/20 px-3 py-1.5 text-sm font-medium text-white hover:bg-rose-500/30">${esc(t('recovery.discard'))}</button></div>`
      : `<div class="mt-3 flex justify-end gap-2">
          <button type="button" data-act="discard" data-key="${esc(o.key)}" class="rounded-xl px-3 py-1.5 text-sm text-zinc-400 hover:bg-rose-400/10 hover:text-rose-200">${esc(t('recovery.discard'))}</button>
          <button type="button" data-act="recover" data-key="${esc(o.key)}" class="${btnCls.btnPrimary} !px-3 !py-1.5">${esc(t('recovery.recover'))}</button></div>`
  return `<li data-orphan="${esc(o.key)}" class="rounded-xl border border-white/10 bg-ink-950/60 p-4">
    <div class="flex items-start justify-between gap-3"><h3 class="min-w-0 truncate font-medium text-zinc-100 ${o.intent?.title ? '' : 'italic'}">${esc(title)}</h3>
      ${chip(t(o.state === 'recording' ? 'recovery.state_crash' : 'recovery.state_interrupted'), 'bg-amber-400/10 text-amber-300')}</div>
    <div class="mt-2 flex flex-wrap gap-1.5 text-xs">
      ${chip(`${fmtDate(o.started_at)} · ${fmtClock(o.started_at)}`)}${chip(fmtTime(o.duration_s, true), 'bg-white/[0.04] font-mono text-zinc-400')}${chip(fmtBytes(o.size_bytes))}${chip(place(o), 'bg-white/[0.04] text-zinc-500')}</div>
    ${devices ? `<p class="mt-2 truncate font-mono text-[11px] text-zinc-600" title="${esc(devices)}">${esc(devices)}</p>` : ''}
    ${st.error ? `<p class="mt-2 text-xs text-rose-300">${esc(st.error)}</p>` : ''}
    ${actions}</li>`
}

function renderRecovery() {
  const d = ownDialog('recovery')
  if (!d.open) return
  d.innerHTML = `<div class="p-6">
    <h2 class="text-lg font-semibold text-white">${esc(t('recovery.title'))}</h2>
    <p class="mt-1 text-sm text-zinc-500">${esc(t('recovery.intro'))}</p>
    <ul id="recovery-list" class="mt-4 max-h-[55vh] space-y-3 overflow-y-auto">${orphans.map(orphanRow).join('')}</ul>
    <div class="mt-4 flex justify-end"><button type="button" data-act="close" class="${btnCls.btn}">${esc(t('recovery.later'))}</button></div></div>`
}

function showRecovery(list: Orphan[]) {
  orphans = list
  for (const k of [...rows.keys()]) if (!list.some(o => o.key === k)) rows.delete(k)
  const d = ownDialog('recovery')
  if (!list.length) { if (d.open) d.close(); return }
  const fresh = list.some(o => !seen.has(o.key))
  list.forEach(o => seen.add(o.key))
  if (d.open) renderRecovery()
  else if (fresh) { d.showModal(); renderRecovery() }
}

async function recoveryAction(act: string, key: string) {
  const st = rows.get(key) ?? {}
  rows.set(key, st)
  if (act === 'close') { ownDialog('recovery').close(); return }
  if (act === 'discard') st.confirming = true
  else if (act === 'cancel-discard') st.confirming = false
  else if (act === 'recover') {
    st.busy = true; st.error = undefined
    renderRecovery()
    try { await api.recordRecover(key) } catch (e) { st.busy = false; st.error = describeError(e) }
  } else if (act === 'discard-yes') {
    st.confirming = false; st.error = undefined
    try {
      await api.recordDiscard(key)
      // o shell re-emite `record-recovery`; se o evento não vier (mock/atraso), tira a linha daqui mesmo
      showRecovery(orphans.filter(o => o.key !== key))
    } catch (e) { st.error = describeError(e) }
  }
  renderRecovery()
}

export async function initRecovery(): Promise<void> {
  await startHub()
  const d = ownDialog('recovery')
  d.addEventListener('click', e => {
    const b = (e.target as HTMLElement).closest<HTMLElement>('[data-act]')
    if (b) void recoveryAction(b.dataset.act!, b.dataset.key ?? '')
  })
  subscribe('progress', renderRecovery)
  await on<Orphan[]>(REC_EVENTS.recovery, showRecovery)
  // falha de uma recuperação: a linha volta ao normal com o erro (vazio = nada gravado, a pasta some sozinha)
  await on<FinalizeDone>(REC_EVENTS.finalizeDone, r => {
    const st = rows.get(r.key)
    if (!st) return
    st.busy = false
    if (!r.ok && r.error?.code !== 'empty_recording') st.error = describeError(r.error)
    renderRecovery()
  })
  // a corrida do startup: o evento pode ter saído antes de a UI escutar, então pergunta também
  try { showRecovery(await api.recordOrphans()) } catch { /* sem backend de gravação */ }
}

/** Toast de fim de finalização (chamado de main.ts): sucesso com link, vazio neutro, outros erros em vermelho. */
export function finalizeToast(d: FinalizeDone) {
  if (d.ok && d.call) toastLink(t('record.toast_done'), `#/call/${d.call.library_id}/${d.call.call_id}`, t('record.open_call'))
  else if (d.error?.code === 'empty_recording') toast(t('record.toast_empty'))
  else if (d.error) toast(describeError(d.error), 'err')
}
