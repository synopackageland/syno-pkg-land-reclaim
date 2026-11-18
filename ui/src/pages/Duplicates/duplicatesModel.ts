// What the Duplicates lists show and put in the cleanup list, worked out once
// per answer: a group is one row whatever its number of copies; its right
// side names the one kept copy; the row's box adds the group to the cleanup
// list, where cleanup.plan removes the scope's Remove copies.
import type { Component } from 'vue'
import { File, Folder, FolderGit2 } from '@lucide/vue'
import { formatBytes, formatCount, formatPercent, joinParts, locale, t } from '@/i18n'
import type { DuplicateSelection } from '@/stores/basket'
import type { Path } from '@/types'
import { isFolderKind, type DuplicateCategory, type DuplicateCopy, type DuplicateGroup, type KeeperReason } from '@/types/duplicates'

/** The page's one filter. Folder groups have no category: they show under All only. */
export type DuplicateFilter = 'all' | 'photo' | 'video' | 'music'
export const DUPLICATE_FILTERS: DuplicateFilter[] = ['all', 'photo', 'video', 'music']
const CATEGORY: Record<Exclude<DuplicateFilter, 'all'>, DuplicateCategory> = { photo: 'photos', video: 'videos', music: 'music' }
/** The API category of a filter; none for All. */
export const filterCategory = (filter: DuplicateFilter): DuplicateCategory | undefined => filter === 'all' ? undefined : CATEGORY[filter]
/** In-scope copies an expanded row lists before it counts the rest (the API sends at most five). */
export const HERE_MAX = 5

export type DuplicateRow = {
  id: string
  group: DuplicateGroup
  name: string
  icon: Component
  /** Under the name: where the scope's copies are. */
  hereText: string
  keptText: string
  /** The kept copy is in a folder the administrator cannot open. */
  keptWhy: string
  copiesText: string
  copiesWhere: string
  sizeText: string
  canFree: number
  /** Copies the row removes; for a merged row, its members'. */
  removes: number
  /** The groups the row stands for: one, or several identical-folder groups that keep the same copy. */
  members: DuplicateGroup[]
}

/** The date of a copy (RFC 3339), as the list shows it. */
export function copyDate(mtime: string): string {
  const date = new Date(mtime)
  return Number.isNaN(date.getTime()) ? '' : new Intl.DateTimeFormat(locale, { year: 'numeric', month: 'short', day: 'numeric' }).format(date)
}

/** A path as the user knows it: from its shared folder, without the volume. */
export function sharePath(path: string, volume: string): string {
  const root = volume.replace(/\/+$/, '')
  return path.startsWith(`${root}/`) ? path.slice(root.length) : path
}

const baseName = (path: string) => path.replace(/\/+$/, '').split('/').at(-1) ?? path
const parent = (path: string) => path.replace(/\/[^/]*\/?$/, '') || '/'
export const groupName = (group: DuplicateGroup['group']) => group.name.path
/** A row's name: for partly identical folders, the side it removes from. */
export const rowName = (group: DuplicateGroup) => group.partial ? baseName(group.partial.path.path) : groupName(group.group)
/** A File Station target's kind for a copy of the group. */
export const copyKind = (group: DuplicateGroup['group']) => isFolderKind(group.kind) ? 'dir' : 'file'

/** "38 of 39 items same as /media/src/linux-6.6-copy": what a partly identical row has in common. */
export function partialText(group: DuplicateGroup, volume: string): string {
  const partial = group.partial
  if (!partial) return ''
  const counterpart = sharePath(partial.counterpart.path, volume)
  return t('duplicates.row.partial', { count: partial.total_items, same: formatCount(partial.same_items), total: formatCount(partial.total_items), counterpart })
}

/**
 * Where a copy is: a file's folder when the file has the group's name, its
 * whole path when the name differs (the user must see which file stays), and
 * a folder's own path.
 */
export function copyLocation(path: Path, group: DuplicateGroup['group'], volume: string): string {
  const shown = sharePath(path.path, volume)
  return group.kind === 'file' && baseName(path.path) === groupName(group) ? parent(shown) : shown
}

export function keeperReasonText(reason: KeeperReason): string {
  return t(`duplicates.reason.${reason}`)
}

/** The scope's name in sentences: a share, or the volume. */
export function scopeName(volume: string, share: string | null): string {
  return share ?? baseName(volume)
}

/** The scope's copies, kept one included. */
const hereCount = (group: DuplicateGroup) => group.in_scope_total + (group.keeper.in_scope ? 1 : 0)
/** Copies outside the scope, kept one excluded. */
export const elsewhereCount = (group: DuplicateGroup) => group.elsewhere_by_share.reduce((sum, entry) => sum + entry.count, 0)
/** The scope's copies past the five the answer lists. */
export const moreHere = (group: DuplicateGroup) => Math.max(0, group.in_scope_total - group.in_scope.length)

/** The kept copy's path from its shared folder. */
export function keeperPath(group: DuplicateGroup, volume: string): string {
  return sharePath(group.keeper.path.path, volume)
}

/** Where the kept copy is, as a row or a line says it. */
export function keeperText(group: DuplicateGroup, volume: string): string {
  return copyLocation(group.keeper.path, group.group, volume)
}

/** Why the kept copy stays, and its date. */
export function keeperWhy(group: DuplicateGroup): string {
  return joinParts([keeperReasonText(group.keeper.reason), copyDate(group.keeper.mtime)].filter(Boolean), locale)
}

/** Copies that stay: the kept one, those the user kept and those no cleanup moves. Copies in identical folders belong to those. */
export const keptCount = (group: DuplicateGroup) => group.partial ? 1 : Math.max(1, group.group.copies_total - group.removes - group.in_identical_folders)

export function duplicateRow(group: DuplicateGroup, volume: string, scope: string): DuplicateRow {
  const here = hereCount(group)
  const total = group.group.copies_total
  const single = group.in_scope_total === 1 && !group.keeper.in_scope && group.in_scope[0]
  const where = single
    ? copyLocation(group.in_scope[0]!.path, group.group, volume)
    : t('duplicates.row.copiesIn', { count: here, scope })
  const files = group.group.kind === 'folder' && group.group.files != null ? t('duplicates.row.files', { count: group.group.files, files: formatCount(group.group.files) }) : ''
  const partial = !!group.partial
  return {
    id: group.group.id,
    group,
    members: [group],
    name: rowName(group),
    icon: partial ? FolderGit2 : isFolderKind(group.group.kind) ? Folder : File,
    hereText: partial ? partialText(group, volume) : joinParts([where, files].filter(Boolean), locale),
    keptText: keeperText(group, volume),
    keptWhy: keeperWhy(group),
    copiesText: t(partial ? 'duplicates.row.folders' : 'duplicates.row.copies', { count: total }),
    copiesWhere: joinParts([
      here === total - group.in_identical_folders
        ? t('duplicates.row.allIn', { scope })
        : t('duplicates.row.hereElsewhere', { here: formatCount(here), elsewhere: formatCount(total - here - group.in_identical_folders) }),
      ...(group.in_identical_folders ? [t('duplicates.row.inFolders', { count: group.in_identical_folders })] : []),
    ], locale),
    sizeText: t(partial ? 'duplicates.row.sameEach' : 'duplicates.row.each', { size: formatBytes(group.group.size) }),
    canFree: group.can_free,
    removes: group.removes,
  }
}

/**
 * Under a row's name: what a partly identical folder shares, else its
 * copies and where they are; a merged row adds how many groups it holds,
 * which is what the cleanup list counts.
 */
export function rowDetail(row: DuplicateRow): string {
  const detail = row.group.partial ? row.hereText : joinParts([row.copiesText, row.copiesWhere].filter(Boolean), locale)
  if (row.members.length < 2) return detail
  return joinParts([detail, t('duplicates.merged.groups', { count: row.members.length })].filter(Boolean), locale)
}

/** A merged row's key: no group id starts with it. */
export const MERGED = 'merged:'

/**
 * One row per kept folder: identical-folder rows that keep the very same
 * copy (two folders that each match it, wholly or partly) become one row
 * whose members are those groups. Rows keep their order, the merged one at
 * its first member's place. Files are never merged: a file group already
 * lists all its copies.
 */
export function mergeRows(rows: DuplicateRow[], volume: string): DuplicateRow[] {
  const byKeeper = new Map<string, DuplicateRow[]>()
  for (const row of rows) {
    if (!isFolderKind(row.group.group.kind)) continue
    const key = row.group.keeper.path.path_hex ?? row.group.keeper.path.path
    byKeeper.set(key, [...byKeeper.get(key) ?? [], row])
  }
  const out: DuplicateRow[] = []
  const done = new Set<string>()
  for (const row of rows) {
    const key = row.group.keeper.path.path_hex ?? row.group.keeper.path.path
    const same = isFolderKind(row.group.group.kind) ? byKeeper.get(key)! : [row]
    if (same.length === 1) {
      out.push(row)
      continue
    }
    if (done.has(key)) continue
    done.add(key)
    const members = same.map(entry => entry.group)
    const removes = members.reduce((sum, group) => sum + group.removes, 0)
    out.push({
      ...row,
      id: `${MERGED}${key}`,
      members,
      name: baseName(row.group.keeper.path.path),
      icon: Folder,
      hereText: t('duplicates.merged.match', { count: members.length, names: joinParts(members.map(rowName), locale) }),
      keptText: keeperText(row.group, volume),
      copiesText: t('duplicates.row.folders', { count: members.length + 1 }),
      copiesWhere: '',
      sizeText: '',
      canFree: members.reduce((sum, group) => sum + group.can_free, 0),
      removes,
    })
  }
  return out
}

/** A merged row's member: the folder it removes from, how much of it matches the kept one, and when it changed. */
export type MergedMember = { group: DuplicateGroup; path: Path; mtime: string; same: string }
export function mergedMember(group: DuplicateGroup): MergedMember {
  const path = group.partial?.path ?? group.in_scope.find(copy => copy.role === 'remove')?.path ?? group.in_scope[0]?.path ?? group.keeper.path
  const copy = group.in_scope.find(entry => entry.path.path === path.path)
  const partial = group.partial
  return {
    group,
    path,
    mtime: copy?.mtime ?? '',
    same: partial
      ? t('duplicates.merged.ratio', { percent: formatPercent(partial.same_bytes / Math.max(1, partial.total_bytes)), same: formatCount(partial.same_items), total: formatCount(partial.total_items) })
      : t('duplicates.merged.identical'),
  }
}

/** Why a group cannot go in the cleanup list, and what of it stays. */
export type GroupCheck = {
  /** Null when the group can go in the list. */
  block: null | { why: 'nothing' }
  /** Copies that stay because no cleanup can move them (the kept one included). */
  unmovable: Path[]
}

/**
 * The one rule both a row's box and Select all follow. A share without a
 * Recycle Bin, or a folder DSM does not list as one, never blocks: its
 * copies are removed for good once Review confirms that (2026-11-14).
 */
export function checkGroup(group: DuplicateGroup): GroupCheck {
  const unmovable = [...group.keeper.reason === 'unmovable' ? [group.keeper.path] : [], ...group.unmovable ?? []]
  return { block: group.removes ? null : { why: 'nothing' }, unmovable }
}

/** DSM's share list with each Recycle Bin; null when DSM did not answer. */
export type Bins = ReadonlyMap<string, { enabled: boolean }> | null

/** The shares a group removes copies from whose Recycle Bin is off: those copies are deleted for good. */
export function offShares(group: DuplicateGroup, bins: Bins): string[] {
  return bins ? (group.remove_shares ?? []).filter(share => bins.get(share)?.enabled === false) : []
}

/**
 * A copy with no Recycle Bin to move to, so removing it deletes it for good:
 * in a share whose Recycle Bin is off, or in a folder below the volume that
 * DSM does not list as a shared folder.
 */
export function deletedForGood(copy: DuplicateCopy, bins: Bins): boolean {
  return !!bins && !!copy.share && (bins.get(copy.share)?.enabled ?? false) === false
}

/** What removing some copies does: all move to a Recycle Bin, all go for good, or some of each. */
export type Removal = 'move' | 'delete' | 'mixed'
/** The shared folder a path is in: its first folder below the volume. */
export const shareOf = (path: string, volume: string) => sharePath(path, volume).split('/').filter(Boolean)[0] ?? null
/**
 * What removing copies in `shares` does, by their Recycle Bins: a share
 * DSM did not list, or whose Recycle Bin is off, deletes for good. Null
 * bins (DSM did not answer) or no shares: they move, as the plan checks again.
 */
export function removalOf(shares: readonly (string | null | undefined)[], bins: Bins): Removal {
  if (!bins || !shares.length) return 'move'
  const off = shares.filter(share => !share || (bins.get(share)?.enabled ?? false) === false).length
  return off === 0 ? 'move' : off === shares.length ? 'delete' : 'mixed'
}

/** Why a copy stays, in words: a shared folder itself, or a folder that is not one. */
export function unmovableText(path: Path, volume: string, bins: ReadonlyMap<string, unknown> | null): string {
  const name = sharePath(path.path, volume).replace(/^\/+/, '')
  return bins?.has(name) ? t('duplicates.stays.shareRoot') : bins ? t('duplicates.stays.notShared') : t('duplicates.stays.belowVolume')
}

/**
 * The cleanup-list entry of a group: cleanup.plan gets its id; the rest is
 * what the cleanup bar and Review show until the plan answers. The copies
 * removed are the scope's copies the user did not keep; only the first five
 * have a role in the answer, so a keep among the others is counted by the
 * plan, not here.
 */
/** Whether the row has a copy to remove here: one whose copies here the user all kept has none. */
export function removable(group: DuplicateGroup): boolean {
  return group.removes > 0
}

export function groupSelection(group: DuplicateGroup, volume: string): DuplicateSelection {
  return {
    id: group.group.id,
    name: rowName(group),
    copies: group.removes,
    canFree: group.can_free,
    freesLater: group.frees_later,
    keeps: keeperPath(group, volume),
    preferred: group.keeper.reason === 'preferred_share',
    drive: group.touches_drive,
    ...(group.partial ? { partial: { items: group.group.same_items ?? group.partial.same_items } } : {}),
    ...(group.remove_shares ? { shares: group.remove_shares } : {}),
  }
}

/** A copy's keep change to send to `duplicates.keep`. */
export type KeepChange = { path: Path; keep: boolean }

/**
 * A copy no cleanup moves: a folder or file right below the volume, a shared
 * folder itself or outside every one (the backend's `unmovable` role). A
 * partly identical side is never one: only its identical items move.
 */
export function stays(copy: DuplicateCopy, group: DuplicateGroup, volume: string): boolean {
  if (copy.role === 'unmovable') return true
  if (group.group.kind === 'folder_partial') return false
  return sharePath(copy.path.path, volume).split('/').filter(Boolean).length === 1
}

/** Why a copy in the sheet has no box: it stays whatever is chosen, or another group moves it. */
export type CopyBlock = 'unmovable' | 'inFolder'
export function copyBlock(copy: DuplicateCopy, group: DuplicateGroup, volume: string): CopyBlock | null {
  if (stays(copy, group, volume)) return 'unmovable'
  if (copy.role === 'in_folder') return 'inFolder'
  return null
}

export const copyKey = (path: Path) => path.path_hex ?? path.path

/**
 * What the sheet's Apply stores so that the copies left unticked are
 * exactly the ones kept (docs/duplicates.md, "Keep rule"). Copies the user
 * keeps replace the rule's kept copy, so leaving only the rule's kept copy
 * unticked stores nothing. Copies no cleanup moves stay without a choice:
 * when every other copy is ticked, one of them is kept by name, so no
 * movable copy is left as the kept one. `kept` holds the keys of the copies
 * left unticked. Null when no copy would stay.
 */
export function keepPlan(group: DuplicateGroup, copies: DuplicateCopy[], kept: ReadonlySet<string>, volume: string): KeepChange[] | null {
  const fixed = copies.filter(copy => stays(copy, group, volume))
  const choosable = copies.filter(copy => copy.role !== 'in_folder' && !fixed.includes(copy))
  const wanted = choosable.filter(copy => kept.has(copyKey(copy.path)))
  if (!wanted.length && !fixed.length) return null
  const ruleKeeper = group.keeper.reason === 'user' ? undefined : copies.find(copy => copy.role === 'keep')
  let target: DuplicateCopy[]
  if (wanted.length) target = wanted.length === 1 && wanted[0] === ruleKeeper ? [] : wanted
  else target = ruleKeeper && fixed.includes(ruleKeeper) ? [] : [fixed[0]!]
  const userKept = copies.filter(copy => copy.role === 'keep_user' || (copy.role === 'keep' && !ruleKeeper))
  return [
    ...target.filter(copy => !userKept.includes(copy)).map(copy => ({ path: copy.path, keep: true })),
    ...userKept.filter(copy => !target.includes(copy)).map(copy => ({ path: copy.path, keep: false })),
  ]
}
