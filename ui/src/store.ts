import type { Bootstrap, ClientInfo, LibraryInfo } from './api'
import { t } from './i18n'

export interface Store {
  boot: Bootstrap
  libraries: LibraryInfo[]
  clients: Map<number, ClientInfo[]>
}

export const store: Store = { boot: null as unknown as Bootstrap, libraries: [], clients: new Map() }

/** Cada tela devolve `refresh` (dados mudaram fora dela) e `dispose` (saiu da tela). */
export interface View {
  refresh?: () => Promise<void> | void
  dispose?: () => void
  /** true enquanto o usuário edita algo: a atualização automática espera. */
  busy?: () => boolean
}

export const meName = () => store.boot.settings.me_name

export const libName = (l: Pick<LibraryInfo, 'kind' | 'name'>) => (l.kind === 'inbox' ? t('nav.unclassified') : l.name)

/** Preenchido por main.ts; as telas chamam para atualizar a barra lateral sem importar main. */
export const hooks = { reloadNav: async () => {} }
