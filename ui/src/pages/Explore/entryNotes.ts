import type { ExploreEntry } from './treemap'

/**
 * Every code `synoreclaim inspect --explain` can give a path (src/document.rs
 * `explain`, with the blocker names of src/model/accounting.rs). The
 * catalogs test reads both Rust sources and fails on a code missing here or
 * a message missing in any locale.
 */
export const NOTICE_CODES = [
  'snapshot',
  'live_reflink',
  'hard_link',
  'other_subvolume',
  'shared_tree',
  'pending_subvolume_deletion',
  'orphan_inode',
  'unknown',
  'unresolved_references',
  'pinned_unreferenced',
  'hard_links',
  'snapshots_not_materialized',
  'unsupported_encrypted',
  'system',
  'dsm_service',
  'reference_accounting_partial',
  'consistency',
] as const

export type NoticeCode = (typeof NOTICE_CODES)[number]

/**
 * What is worth knowing about one row beyond its columns, worked out from the
 * row the list already holds; nothing is asked of the NAS. Report-wide notes
 * (unread snapshots, partial accounting, a changing volume) stay in the page
 * status, so a row has notes only when they are about that row.
 */
export function entryNotes(entry: ExploreEntry): NoticeCode[] {
  // Unresolved bytes say more than the bare `unknown` blocker, so one line stands for both.
  const unresolved = entry.metrics.unknown > 0
  const notes: NoticeCode[] = entry.blockers.filter(blocker => !(unresolved && blocker === 'unknown'))
  if (unresolved) notes.push('unresolved_references')
  if ((entry.metrics.pinned_unreferenced ?? 0) > 0) notes.push('pinned_unreferenced')
  if (entry.nlink > 1) notes.push('hard_links')
  if (entry.flags.includes('encrypted')) notes.push('unsupported_encrypted')
  return [...new Set(notes)]
}

/** Why cleanup refuses a row the NAS calls ineligible, as src/guard.rs protects it. */
export type ProtectedKind = 'recycle_bin' | 'dsm_folder' | 'share_root' | 'system' | 'own_state'

/**
 * The protection behind a row the NAS marks `cleanup_eligible: false` while its
 * fix still reads `cleanup`: a Recycle Bin, a DSM-managed `@` folder, a shared
 * folder itself, DSM or Syno Reclaim data. Null when the row says nothing of it.
 */
export function protectedKind(path: string, mount: string, flags: readonly string[]): ProtectedKind | null {
  const base = mount.replace(/\/+$/, '')
  const rel = path.startsWith(`${base}/`) ? path.slice(base.length + 1) : path.replace(/^\/+/, '')
  const parts = rel.split('/').filter(Boolean)
  if (flags.includes('recycle') || parts.includes('#recycle')) return 'recycle_bin'
  if (flags.includes('own_state')) return 'own_state'
  // Cleanup refuses any path with an @ folder in it (guard::protected_names), not only one right below the volume.
  if (parts.some(part => part.startsWith('@'))) return 'dsm_folder'
  if (flags.includes('system')) return 'system'
  if (parts.length === 1 || flags.includes('subvolume_root')) return 'share_root'
  return null
}
