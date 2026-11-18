// Duplicates answers for the dev server and tests: the WebAPI contract of
// docs/duplicates.md, worked out from a typed list of groups. It applies the
// keep rule itself, so Keep, preferred shares, scopes, copies the administrator
// cannot open, recalculation and the analysis states behave as the real
// API does. Wrapped around the recorded fixtures by fixtureBackend(); a
// production build never loads it.
import type { Backend, Method, ParamsOf } from './methods'
import type { Path } from '@/types'
import type {
  DuplicateAmount, DuplicateAnalysis, DuplicateCategory, DuplicateCopiesDocument, DuplicateCopy, DuplicateGroup,
  DuplicateItem, DuplicateKeepDocument, DuplicateKind, DuplicatePartial, DuplicatePreferredDocument, DuplicateRole,
  DuplicateScopeDocument, DuplicateTally, KeeperReason,
} from '@/types/duplicates'
import { isFolderKind } from '@/types/duplicates'

const GiB = 1024 ** 3
const MiB = 1024 ** 2

/** `side`: a side of partly identical folders, with the whole folder's counts. */
type MockCopy = { at: string; date: string; name?: string; snapshot?: boolean; drive?: boolean; side?: { items: number; files: number; bytes: number } }
/** An identical item of partly identical folders: a path below each side. */
type MockItem = { rel: string; folder: boolean; files: number; bytes: number }
export type MockGroup = {
  key: string
  kind: DuplicateKind
  name: string
  size: number
  files?: number
  category?: DuplicateCategory
  copies: MockCopy[]
  /** `folder_partial`: the identical items, the same below every side. */
  items?: MockItem[]
}

export const copy = (at: string, date: string, extra: Omit<MockCopy, 'at' | 'date'> = {}): MockCopy => ({ at, date, ...extra })

/** A contract-shaped group id (`^f?[0-9a-f]{32}$`) for a mock group's key. */
export function mockId(key: string, kind: DuplicateKind = 'file'): string {
  let hash = 0x811c9dc5
  let hex = ''
  for (let round = 0; hex.length < 32; round++) {
    for (const char of `${key}#${round}`) hash = Math.imul(hash ^ char.charCodeAt(0), 0x01000193) >>> 0
    hex += hash.toString(16).padStart(8, '0')
  }
  return isFolderKind(kind) ? `f${hex.slice(0, 32)}` : hex.slice(0, 32)
}

// A Linux v6.6 clone and an export of its worktree without .git: 38 of the
// clone's 39 entries are the same, so the pair is one partly identical row.
const KERNEL_FOLDERS: [string, number, number][] = [
  ['drivers', 33_210, 640], ['arch', 18_420, 150], ['Documentation', 9_880, 68], ['tools', 5_120, 42],
  ['fs', 2_610, 58], ['include', 6_080, 52], ['sound', 2_640, 44], ['net', 2_000, 40], ['kernel', 560, 12],
  ['scripts', 590, 8], ['lib', 760, 9], ['mm', 160, 6], ['security', 420, 5], ['crypto', 230, 5],
  ['rust', 140, 2], ['block', 100, 2], ['samples', 520, 2], ['io_uring', 50, 1], ['init', 20, 1],
  ['ipc', 20, 1], ['virt', 40, 1], ['certs', 20, 1], ['usr', 20, 1], ['LICENSES', 90, 1],
]
const KERNEL_FILES: [string, number][] = [
  ['MAINTAINERS', 760_000], ['CREDITS', 100_000], ['Makefile', 66_000], ['COPYING', 18_000], ['.mailmap', 50_000],
  ['Kconfig', 600], ['Kbuild', 3_000], ['README', 700], ['.gitignore', 2_000], ['.clang-format', 20_000],
  ['.cocciconfig', 100], ['.gitattributes', 100], ['.get_maintainer.ignore', 100], ['.rustfmt.toml', 500],
]
const KERNEL_ITEMS: MockItem[] = [
  ...KERNEL_FOLDERS.map(([rel, files, mib]) => ({ rel, folder: true, files, bytes: mib * MiB })),
  ...KERNEL_FILES.map(([rel, bytes]) => ({ rel, folder: false, files: 1, bytes })),
]
const kernelBytes = KERNEL_ITEMS.reduce((sum, item) => sum + item.bytes, 0)
const kernelFiles = KERNEL_ITEMS.reduce((sum, item) => sum + item.files, 0)

/** The design's cases, plus a few groups so every share has some. */
export const MOCK_GROUPS: MockGroup[] = [
  { key: 'ubuntu', kind: 'file', name: 'ubuntu-24.04.1-desktop-amd64.iso', size: 6.1 * GiB, category: 'other', copies: [
    copy('/software/os/ubuntu', '2024-09-12'),
    copy('/Archive/iso', '2025-01-03'),
    copy('/Archive/iso/old', '2024-11-20'),
    copy('/Archive/old-mac/Downloads', '2025-02-14'),
    copy('/Archive/usb-stick-2024', '2024-10-02'),
    copy('/Archive/usb-stick-2025', '2025-06-30'),
    copy('/Archive/to-sort', '2026-02-11'),
    copy('/homes/alice/Downloads', '2025-03-08'),
    copy('/homes/alice/iso', '2025-03-09'),
    copy('/homes/alice/old-laptop/Downloads', '2024-12-30'),
    copy('/homes/bob/Downloads', '2025-04-17'),
    copy('/homes/bob/vm/iso', '2025-04-18'),
    copy('/homes/bob/Desktop', '2025-05-02'),
    copy('/homes/bob/usb', '2025-06-30'),
    copy('/homes/carol/Downloads', '2025-08-21'),
    copy('/homes/carol/install', '2025-08-22'),
    copy('/software/os/ubuntu/24.04', '2024-09-13'),
    copy('/public/installers', '2025-01-15'),
    copy('/public/it/os', '2025-07-07'),
    copy('/video/tmp', '2025-10-19'),
  ] },
  { key: 'family', kind: 'file', name: 'family-2019.mkv', size: 12.4 * GiB, category: 'videos', copies: [
    copy('/video/home-movies', '2020-01-04'),
    copy('/Archive/dvd-rips', '2023-05-27', { snapshot: true }),
  ] },
  { key: 'render', kind: 'file', name: 'render-final.mov', size: 4.3 * GiB, category: 'videos', copies: [
    copy('/Archive/projects/2025', '2025-11-04'),
    copy('/Archive/projects/2025/old', '2025-11-18'),
    copy('/Archive/to-sort', '2026-01-09'),
  ] },
  { key: 'img4471', kind: 'file', name: 'IMG_4471.MOV', size: 1.8 * GiB, category: 'videos', copies: [
    copy('/photo/2026/07', '2026-07-19'),
    copy('/Archive/camera-roll', '2026-07-21'),
    copy('/Archive/phone-backup-2025/DCIM/100APPLE', '2026-08-02'),
    copy('/homes/alice/Videos/trip', '2026-07-22'),
    copy('/private/phone', '2026-07-25'),
    copy('/homes/bob/Downloads', '2026-09-01'),
  ] },
  { key: 'dsc0192', kind: 'file', name: 'DSC_0192 (1).ARW', size: 48 * MiB, category: 'photos', copies: [
    copy('/photo/2025/raw', '2025-04-02', { name: 'DSC_0192.ARW' }),
    copy('/Archive/raw', '2025-04-09'),
  ] },
  { key: 'album', kind: 'file', name: 'live-at-the-hall.flac', size: 640 * MiB, category: 'music', copies: [
    copy('/music/live', '2021-03-02'),
    copy('/homes/carol/Music', '2022-06-11'),
  ] },
  { key: 'deck', kind: 'file', name: 'q3-review.key', size: 86 * MiB, category: 'other', copies: [
    copy('/homes/alice/Drive/work', '2026-09-30', { drive: true }),
    copy('/homes/bob/Drive/shared', '2026-10-01', { drive: true }),
  ] },
  { key: 'scans', kind: 'file', name: 'tax-scans-2018.zip', size: 220 * MiB, category: 'other', copies: [
    copy('/private/finance', '2018-04-01'),
    copy('/Archive/to-sort', '2023-02-10'),
    copy('/homes/alice/Downloads', '2024-05-06'),
  ] },
  // Two copies lie inside copies of the identical folder wedding-2022: that
  // folder group removes them (role in_folder); this row lists the third.
  { key: 'vows', kind: 'file', name: 'vows.mov', size: 2.2 * GiB, category: 'videos', copies: [
    copy('/photo/2022/wedding', '2022-10-15'),
    copy('/Archive/wedding-2022', '2022-10-15'),
    copy('/homes/bob/Videos', '2023-01-02'),
  ] },
  // A folder of small files (a source tree): tens of thousands of files.
  { key: 'src', kind: 'folder', name: 'reclaim-src', size: 1.2 * GiB, files: 48_213, copies: [
    copy('/homes/alice/dev/reclaim-src', '2026-03-02'),
    copy('/Archive/old-mac/dev/reclaim-src', '2026-03-02'),
    copy('/public/backup/reclaim-src', '2026-03-05'),
  ] },
  { key: 'wedding', kind: 'folder', name: 'wedding-2022', size: 31 * GiB, files: 642, copies: [
    copy('/photo/2022/wedding', '2022-10-15'),
    copy('/Archive/wedding-2022', '2022-10-15'),
  ] },
  // Partly identical folders: the clone keeps .git, the export has none.
  { key: 'linux', kind: 'folder_partial', name: 'linux-6.6', size: kernelBytes, files: kernelFiles, items: KERNEL_ITEMS, copies: [
    copy('/homes/alice/src/linux-6.6', '2026-01-10', { side: { items: 39, files: kernelFiles + 9_800, bytes: kernelBytes + 2.4 * GiB } }),
    copy('/Archive/src/linux-6.6-copy', '2025-12-01', { side: { items: 38, files: kernelFiles, bytes: kernelBytes } }),
  ] },
  // A second folder that matches the same kept folder: its row merges with linux-6.6's.
  { key: 'linuxenv', kind: 'folder_partial', name: 'linux-6.6-copy-env', size: kernelBytes, files: kernelFiles, items: KERNEL_ITEMS, copies: [
    copy('/homes/alice/src/linux-6.6-copy-env', '2026-03-01', { side: { items: 40, files: kernelFiles + 120, bytes: kernelBytes + 80 * MiB } }),
    copy('/Archive/src/linux-6.6-copy', '2025-12-01', { side: { items: 38, files: kernelFiles, bytes: kernelBytes } }),
  ] },
  { key: 'phone', kind: 'folder', name: 'phone-alice', size: 9.4 * GiB, files: 1284, copies: [
    copy('/photo/phone/alice', '2026-10-30'),
    copy('/Archive/phone-alice', '2026-10-30'),
    copy('/Archive/old-mac/Pictures/phone-alice', '2026-10-30'),
    copy('/homes/alice/Photos/phone-alice', '2026-10-30'),
  ] },
]

export type DuplicatesMockOptions = {
  groups?: MockGroup[]
  preferred?: string[]
  /** How long an analysis runs. */
  analyzeMs?: number
  /** The analysis as the page first finds it; `unavailable` answers as a volume without duplicate data. */
  analysis?: DuplicateAnalysis['state'] | 'unavailable'
  /** With `analysis: 'unavailable'`: the reason the backend gives instead of a missing scan. */
  unavailableMessage?: string
  /** The analysis job ends failed, as on a report scanned before candidates were recorded; `job.result` says why. */
  analyzeFails?: boolean
}

const DUPLICATE_METHODS = new Set<Method>(['duplicates.analyze', 'duplicates.scope', 'duplicates.copies', 'duplicates.keep', 'duplicates.preferred_shares'])

const rfc3339 = (date: string) => `${date}T12:00:00Z`
const shareOf = (at: string) => at.split('/')[1]!
const text = (value: string | Path | undefined) => typeof value === 'string' ? value : value?.path
const json = (path: string): Path => ({ path })
const within = (path: string, scope: string) => path === scope || path.startsWith(`${scope.replace(/\/+$/, '')}/`)
const zero = (): DuplicateAmount => ({ copies: 0, can_free: 0, frees_later: 0 })
const emptyTally = (): DuplicateTally => ({ all: zero(), files: zero(), photos: zero(), videos: zero(), music: zero(), folders: zero() })

const envelope = <C extends string>(command: C) => ({
  schema_version: 1 as const,
  command,
  tool_version: '0.1.0',
  generated_at: new Date().toISOString(),
  completion: 'complete' as const,
  job_id: null,
  errors: [],
  warnings: [],
})

export function createDuplicatesMock(options: DuplicatesMockOptions = {}) {
  const groups = options.groups ?? MOCK_GROUPS
  const preferred = new Map<string, string[]>()
  const userKeeps = new Map<string, Set<string>>()
  let state: DuplicateAnalysis['state'] | 'unavailable' = options.analysis ?? 'ready'
  let runningUntil = 0
  let changedShares = false
  let analyses = 0
  const running = () => Date.now() < runningUntil
  // An analysis that has finished is the one the answers use.
  const settle = () => {
    if (runningUntil && !running()) {
      runningUntil = 0
      if (!options.analyzeFails) state = 'ready'
      changedShares = false
    }
  }
  const preferredOf = (volume: string) => preferred.get(volume) ?? options.preferred ?? ['photo']
  const keepsOf = (id: string) => userKeeps.get(id) ?? new Set<string>()
  const fullPath = (volume: string, group: MockGroup, item: MockCopy) => isFolderKind(group.kind)
    ? `${volume}${item.at}`
    : `${volume}${item.at}/${item.name ?? group.name}`

  /** The keep rule of docs/duplicates.md, then each copy's role. */
  function view(volume: string, group: MockGroup, scope: string) {
    const id = mockId(group.key, group.kind)
    const paths = group.copies.map(item => fullPath(volume, group, item))
    const inScope = group.copies.map((_, index) => within(paths[index]!, scope))
    const keeps = keepsOf(id)
    // A file copy inside a copy of a listed identical folder belongs to that folder group.
    const folderCopies = group.kind === 'file' ? groups.filter(entry => entry.kind === 'folder').flatMap(entry => entry.copies.map(item => fullPath(volume, entry, item))) : []
    const inFolder = paths.map(path => folderCopies.some(folder => within(path, folder) && path !== folder))
    const byAge = (left: number, right: number) => group.copies[left]!.date.localeCompare(group.copies[right]!.date) || paths[left]!.length - paths[right]!.length
    const indexes = group.copies.map((_, index) => index)
    const shares = preferredOf(volume)
    let keeper: number
    let reason: KeeperReason
    const kept = indexes.filter(index => keeps.has(paths[index]!)).sort(byAge)
    const preferredCopies = indexes
      .filter(index => shares.includes(shareOf(group.copies[index]!.at)))
      .sort((left, right) => shares.indexOf(shareOf(group.copies[left]!.at)) - shares.indexOf(shareOf(group.copies[right]!.at)) || byAge(left, right))
    // A folder or file right below the volume (a shared folder itself, or outside every one) is never moved.
    const fixed = paths.map(path => !group.items && path.slice(volume.length).split('/').filter(Boolean).length === 1)
    const unmovable = indexes.filter(index => fixed[index] && !inFolder[index]).sort(byAge)
    if (kept[0] !== undefined) [keeper, reason] = [kept[0], 'user']
    else if (preferredCopies[0] !== undefined) [keeper, reason] = [preferredCopies[0], 'preferred_share']
    else if (unmovable[0] !== undefined) [keeper, reason] = [unmovable[0], 'unmovable']
    else {
      const sorted = [...indexes].sort(byAge)
      const tied = sorted.length > 1 && group.copies[sorted[0]!]!.date === group.copies[sorted[1]!]!.date
      ;[keeper, reason] = [sorted[0]!, tied ? 'shortest' : 'oldest']
    }
    const roles: DuplicateRole[] = indexes.map(index => index === keeper
      ? 'keep'
      : keeps.has(paths[index]!) ? 'keep_user' : inFolder[index] ? 'in_folder' : fixed[index] ? 'unmovable' : 'remove')
    return { id, paths, inScope, keeper, reason, roles }
  }

  function row(volume: string, group: MockGroup, scope: string): DuplicateGroup | null {
    const { id, paths, inScope, keeper, reason, roles } = view(volume, group, scope)
    // A scope lists the groups with a copy in it; each is seen on the whole volume.
    if (!inScope.some(Boolean)) return null
    const removed = indexesOf(roles, 'remove')
    // A group whose copies here the user all kept stays, so the choice can be undone.
    if (!removed.length && !keepsOf(id).size) return null
    const here = group.copies.map((item, index) => ({ item, index })).filter(({ index }) => index !== keeper && inScope[index] && roles[index] !== 'in_folder')
    const counts = new Map<string, number>()
    group.copies.forEach((item, index) => {
      if (index === keeper || roles[index] === 'in_folder' || inScope[index]) return
      counts.set(shareOf(item.at), (counts.get(shareOf(item.at)) ?? 0) + 1)
    })
    const keptCopy = group.copies[keeper]!
    const later = removed.filter(index => group.copies[index]!.snapshot).length
    return {
      group: {
        id,
        kind: group.kind,
        name: json(group.name),
        size: group.size,
        ...(group.files ? { files: group.files } : {}),
        copies_total: group.copies.length,
        ...(group.kind === 'file' ? { category: group.category ?? 'other' } : {}),
        ...(group.items ? { same_items: group.items.length } : {}),
      },
      keeper: { path: json(paths[keeper]!), share: shareOf(keptCopy.at), mtime: rfc3339(keptCopy.date), reason, in_scope: inScope[keeper]! },
      in_scope: here.slice(0, 5).map(({ item, index }) => ({
        path: json(paths[index]!),
        share: shareOf(item.at),
        mtime: rfc3339(item.date),
        role: roles[index] as 'remove' | 'keep_user' | 'unmovable',
      })),
      in_scope_total: here.length,
      removes: removed.length,
      elsewhere_by_share: [...counts].map(([share, count]) => ({ share, count })).sort((left, right) => right.count - left.count || left.share.localeCompare(right.share)),
      remove_shares: [...new Set(removed.map(index => shareOf(group.copies[index]!.at)))].sort(),
      unmovable: indexesOf(roles, 'unmovable').map(index => json(paths[index]!)),
      in_identical_folders: indexesOf(roles, 'in_folder').length,
      can_free: (removed.length - later) * group.size,
      frees_later: later * group.size,
      touches_drive: removed.some(index => group.copies[index]!.drive),
      shared: false,
      ...(group.items ? { partial: partialOf(group, paths, removed, inScope, keeper) } : {}),
    }
  }

  /** The side a partly identical row removes from first (in the scope first), and the kept side. */
  function partialOf(group: MockGroup, paths: string[], removed: number[], inScope: boolean[], keeper: number): DuplicatePartial {
    const index = [...removed].sort((left, right) => Number(inScope[right]) - Number(inScope[left]))[0]!
    const side = group.copies[index]!.side!
    return {
      path: json(paths[index]!),
      same_items: group.items!.length,
      total_items: side.items,
      same_files: group.files ?? 0,
      total_files: side.files,
      same_bytes: group.size,
      total_bytes: side.bytes,
      counterpart: json(paths[keeper]!),
    }
  }

  /** A partly identical group's identical items, largest first: one page. */
  function itemsPage(group: MockGroup, offset: number, limit: number) {
    const order = [...group.items!].sort((left, right) => right.bytes - left.bytes || left.rel.localeCompare(right.rel))
    const items: DuplicateItem[] = order.slice(offset, offset + limit).map(item => ({ path: json(item.rel), kind: item.folder ? 'folder' : 'file', files: item.files, bytes: item.bytes }))
    return { total: order.length, offset, items }
  }

  function tally(volume: string, scope: string): DuplicateTally {
    const sum = emptyTally()
    for (const group of groups) {
      const entry = row(volume, group, scope)
      if (!entry) continue
      const removes = indexesOf(view(volume, group, scope).roles, 'remove').length
      const kinds: (keyof DuplicateTally)[] = isFolderKind(group.kind) ? ['all', 'folders'] : ['all', 'files', ...(group.category && group.category !== 'other' ? [group.category] : [])]
      for (const kind of kinds) {
        sum[kind].copies += removes
        sum[kind].can_free += entry.can_free
        sum[kind].frees_later += entry.frees_later
      }
    }
    return sum
  }

  function analysis(): DuplicateAnalysis {
    settle()
    return {
      state: state === 'unavailable' ? 'none' : state,
      running: running(),
      job_id: running() ? `duplicates-analyze-${analyses}` : null,
      analyzed_at: state === 'none' ? null : '2026-11-12T08:00:00Z',
      report_job_id: state === 'none' ? null : 'scan-fixture',
      complete: true,
    }
  }

  function unavailable(command: string, volume: string) {
    return {
      ...envelope(command),
      completion: 'failed' as const,
      errors: [{ path: json(volume), code: 'duplicates_unavailable', message: options.unavailableMessage ?? 'no duplicate candidates: scan this volume again' }],
    }
  }

  function scope(params: ParamsOf<'duplicates.scope'>): DuplicateScopeDocument {
    const volume = text(params.volume)!
    const path = text(params.path) ?? volume
    if (state === 'unavailable') return unavailable('duplicates-scope', volume) as never
    const status = analysis()
    const list = params.list ?? 'files'
    const base = {
      ...envelope('duplicates-scope'),
      volume: json(volume),
      scope: json(path),
      analysis: status,
      recalculating: changedShares || status.running,
      thresholds: { record_bytes: MiB, list_bytes: 10 * MiB },
      preferred_shares: preferredOf(volume),
      list,
      category: params.category ?? null,
    }
    if (state === 'none') {
      return { ...base, totals: null, children: [], unchecked: null, touches_drive: false, groups: { total: 0, offset: params.offset ?? 0, items: [] } }
    }
    const rowsOf = (folders: boolean, category?: string) => groups
      .filter(group => folders ? isFolderKind(group.kind) : group.kind === 'file')
      .filter(group => !category || group.category === category)
      .map(group => row(volume, group, path))
      .filter((entry): entry is DuplicateGroup => !!entry)
      .sort((left, right) => right.can_free - left.can_free || right.frees_later - left.frees_later)
    const rows = rowsOf(list === 'folders', params.category)
    const folderRows = list === 'both' ? rowsOf(true) : []
    const children = [...new Set(groups.flatMap(group => group.copies
      .map(item => fullPath(volume, group, item))
      .filter(full => within(full, path) && full !== path)
      .map(full => `${path.replace(/\/+$/, '')}/${full.slice(path.replace(/\/+$/, '').length + 1).split('/')[0]}`)))]
      .map(child => ({ path: json(child), name: json(child.split('/').at(-1)!), tally: tally(volume, child) }))
      .filter(child => child.tally.all.copies > 0)
      .sort((left, right) => right.tally.all.can_free - left.tally.all.can_free || left.name.path.localeCompare(right.name.path))
    const offset = params.offset ?? 0
    return {
      ...base,
      totals: tally(volume, path),
      children,
      unchecked: { files: 3, bytes: 2.1 * GiB, no_checksums: 2, compressed: 1, folders: 1, folder_bytes: 700 * MiB },
      touches_drive: [...rows, ...folderRows].some(entry => entry.touches_drive),
      groups: { total: rows.length, offset, items: rows.slice(offset, offset + (params.limit ?? 50)) },
      ...(list === 'both' ? { folders: { total: folderRows.length, offset, items: folderRows.slice(offset, offset + (params.limit ?? 50)) } } : {}),
    }
  }

  function copies(params: ParamsOf<'duplicates.copies'>): DuplicateCopiesDocument {
    const volume = text(params.volume)!
    const path = text(params.scope) ?? volume
    if (state === 'unavailable') return unavailable('duplicates-copies', volume) as never
    const group = groups.find(entry => mockId(entry.key, entry.kind) === params.group_id)
    const base = { ...envelope('duplicates-copies'), volume: json(volume), scope: json(path) }
    if (!group) return { ...base, group: null, copies: { total: 0, offset: 0, items: [] } }
    const { paths, inScope, keeper, roles } = view(volume, group, path)
    const scopeShare = shareOf(path.slice(volume.length) || '/')
    const keeperShare = shareOf(group.copies[keeper]!.at)
    const rank = (share: string) => share === scopeShare ? 0 : share === keeperShare ? 1 : 2
    const items: DuplicateCopy[] = group.copies
      .map((item, index) => ({ item, index }))
      .map(({ item, index }) => ({ path: json(paths[index]!), share: shareOf(item.at), mtime: rfc3339(item.date), role: roles[index]!, in_scope: inScope[index]! }))
      .sort((left, right) => rank(left.share) - rank(right.share) || left.share.localeCompare(right.share))
    const offset = params.offset ?? 0
    return {
      ...base,
      group: row(volume, group, path),
      copies: { total: items.length, offset, items: items.slice(offset, offset + (params.limit ?? 50)) },
      ...(group.items ? { items: itemsPage(group, offset, Math.min(params.limit ?? 50, 200)) } : {}),
    }
  }

  function keep(params: ParamsOf<'duplicates.keep'>): DuplicateKeepDocument {
    const volume = text(params.volume)!
    const path = text(params.scope) ?? volume
    const keeps = keepsOf(params.group_id)
    if (params.keep) keeps.add(text(params.path)!)
    else keeps.delete(text(params.path)!)
    userKeeps.set(params.group_id, keeps)
    const group = groups.find(entry => mockId(entry.key, entry.kind) === params.group_id)
    return { ...envelope('duplicates-keep'), volume: json(volume), scope: json(path), group: group ? row(volume, group, path) : null, totals: tally(volume, path) }
  }

  function preferredShares(params: ParamsOf<'duplicates.preferred_shares'>): DuplicatePreferredDocument {
    const volume = text(params.volume)!
    if (params.shares) {
      preferred.set(volume, [...params.shares])
      changedShares = true
    }
    return { ...envelope('duplicates-preferred-shares'), volume: json(volume), shares: preferredOf(volume), recalculating: changedShares || running() }
  }

  function analyze() {
    settle()
    if (!running()) {
      analyses++
      runningUntil = Date.now() + (options.analyzeMs ?? 2500)
    }
    return { ...envelope('detach'), command: 'detach', job_id: `duplicates-analyze-${analyses}`, launcher: 'none' }
  }

  /** The finished analysis job's result document. */
  function analysisResult() {
    settle()
    return options.analyzeFails ? unavailable('duplicates-analyze', '/volume1') : { ...envelope('duplicates-analyze') }
  }

  async function answer(method: Method, params: unknown): Promise<unknown> {
    await new Promise(resolve => setTimeout(resolve, 0))
    if (method === 'duplicates.scope') return scope(params as ParamsOf<'duplicates.scope'>)
    if (method === 'duplicates.copies') return copies(params as ParamsOf<'duplicates.copies'>)
    if (method === 'duplicates.keep') return keep(params as ParamsOf<'duplicates.keep'>)
    if (method === 'duplicates.preferred_shares') return preferredShares(params as ParamsOf<'duplicates.preferred_shares'>)
    if (method === 'job.result') return analysisResult()
    return analyze()
  }

  return { answer, userKeeps, preferred }
}

function indexesOf(roles: DuplicateRole[], role: DuplicateRole) {
  return roles.flatMap((entry, index) => entry === role ? [index] : [])
}

/** The duplicates scenarios of the dev server's `?scenario=`. */
const SCENARIO_ANALYSIS: Partial<Record<string, DuplicatesMockOptions['analysis']>> = {
  duplicates_none: 'none',
  duplicates_stale: 'stale',
  duplicates_unavailable: 'unavailable',
}

/** Answers the duplicates methods from the mock; every other method from `backend`. */
export function withDuplicatesFixture(backend: Backend, options: DuplicatesMockOptions & { scenario?: string } = {}): Backend {
  const { scenario, ...rest } = options
  const mock = createDuplicatesMock({ analysis: scenario ? SCENARIO_ANALYSIS[scenario] : undefined, ...rest })
  return ((method, params, request) => DUPLICATE_METHODS.has(method)
    ? mock.answer(method, params)
    : request ? backend(method, params, request) : backend(method, params)) as Backend
}
