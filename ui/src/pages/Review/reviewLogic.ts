import type { BasketItem, DuplicateSelection, DuplicatesInBasket } from '@/stores/basket'

/** Only the selected basket paths and report identities may be sent to cleanup.plan. */
export function basketPaths(items: readonly BasketItem[]) {
  return items.map(({ path, path_hex, expect }) => ({ path, ...(path_hex ? { path_hex } : {}), ...(expect ? { expect: { ...expect } } : {}) }))
}

/**
 * cleanup.apply receives the sealed plan only; it never receives basket
 * paths. `deletes`: the plan removes items with no Recycle Bin for good,
 * which the user confirmed in the dialog.
 */
export function cleanupApplyParams(planId: string, deletes = false) {
  return { plan_id: planId, confirm_move: true as const, ...(deletes ? { confirm_delete: true as const } : {}) }
}

/**
 * `items`: partly identical folders, whose removed folders each lose only that many identical items.
 * `removal`: what happens to the removed copies: `move` to their Recycle Bins, `delete` for good, or
 * `mixed` when only some of their shared folders have no Recycle Bin, or the group does not name them.
 */
export type DuplicateReviewLine = { group: string; name: string; removes: number; keeps: string; preferred: boolean; removal: DuplicateRemoval; items?: number }
export type DuplicateRemoval = 'move' | 'delete' | 'mixed'

/** What happens to a group's removed copies, given the plan's shared folders that are deleted for good. */
function removal(group: DuplicateSelection, deleteShares: ReadonlySet<string>): DuplicateRemoval {
  if (!deleteShares.size) return 'move'
  if (!group.shares?.length) return 'mixed'
  const deleted = group.shares.filter(share => deleteShares.has(share)).length
  return deleted === 0 ? 'move' : deleted === group.shares.length ? 'delete' : 'mixed'
}

/**
 * The duplicate groups in the cleanup list, one line per group: how many of
 * its copies are removed, whether to the Recycle Bin or for good (the plan's
 * `deleteShares`, by name), and which copy stays; then what a snapshot frees
 * only later, and whether a Drive folder is touched.
 */
export function duplicateReview(groups: readonly DuplicateSelection[], deleteShares: ReadonlySet<string> = new Set()) {
  const lines: DuplicateReviewLine[] = groups.map(group => ({
    group: group.id,
    name: group.name,
    removes: group.copies,
    keeps: group.keeps,
    preferred: group.preferred,
    removal: removal(group, deleteShares),
    ...(group.partial ? { items: group.partial.items } : {}),
  }))
  return {
    lines,
    partial: lines.some(line => line.items != null),
    freesLater: groups.reduce((sum, group) => sum + group.freesLater, 0),
    touchesDrive: groups.some(group => group.drive),
  }
}

/** cleanup.plan's duplicates selection for the list's groups, or nothing. Each group is seen on its whole volume. */
export function duplicatesParams(volume: string, selection: DuplicatesInBasket | null) {
  if (!selection?.groups.length) return {}
  return { duplicates: { volume, groups: selection.groups.map(group => group.id) } }
}

/** cleanup.plan and cleanup.apply refusals that come from a duplicates selection. */
export const DUPLICATE_REFUSALS = new Set(['duplicates_stale', 'duplicates_unknown_group', 'keeper_changed', 'keeper_removed', 'keeper_conflict', 'duplicates_nothing', 'duplicates_unavailable', 'duplicates_invalid'])

/**
 * The most bytes of params one request carries: the executor takes 64 KiB.
 * A larger selection goes to cleanup.select in chunks of this size.
 */
export const CHUNK_BYTES = 48_000

type PlanPath = ReturnType<typeof basketPaths>[number]
export type PlanParams = { paths?: PlanPath[]; duplicates?: { volume: string; groups: string[] }; selection?: string }
type Stage = (params: { selection?: string; paths?: PlanPath[]; groups?: string[] }) => Promise<{ selection: string }>

/**
 * The params of cleanup.plan for the selection: the paths and groups
 * themselves when they fit one request, else a selection staged chunk by
 * chunk with `stage` (cleanup.select), which the plan reads on the NAS.
 */
export async function planParams(items: readonly BasketItem[], volume: string, duplicates: DuplicatesInBasket | null, stage: Stage, signal?: AbortSignal): Promise<PlanParams> {
  const inline: PlanParams = { paths: basketPaths(items), ...duplicatesParams(volume, duplicates) }
  if (JSON.stringify(inline).length <= CHUNK_BYTES) return inline
  const entries: ({ path: PlanPath } | { group: string })[] = [
    ...inline.paths!.map(path => ({ path })),
    ...(duplicates?.groups ?? []).map(group => ({ group: group.id })),
  ]
  let selection: string | undefined
  let chunk = { paths: [] as PlanPath[], groups: [] as string[] }
  let size = 0
  const send = async () => {
    if (signal?.aborted) throw new DOMException('Selection stopped', 'AbortError')
    const answer = await stage({ ...(selection ? { selection } : {}), ...(chunk.paths.length ? { paths: chunk.paths } : {}), ...(chunk.groups.length ? { groups: chunk.groups } : {}) })
    selection = answer.selection
    chunk = { paths: [], groups: [] }
    size = 0
  }
  for (const entry of entries) {
    const bytes = JSON.stringify('path' in entry ? entry.path : entry.group).length + 1
    if (size + bytes > CHUNK_BYTES - 200 && size) await send()
    if ('path' in entry) chunk.paths.push(entry.path)
    else chunk.groups.push(entry.group)
    size += bytes
  }
  if (size) await send()
  const plan: PlanParams = { selection }
  if (inline.duplicates) plan.duplicates = { ...inline.duplicates, groups: [] }
  return plan
}

/** A duplicate group a keeper conflict names. */
export type ConflictGroup = { id: string; name: string }
/** The copy at `path` is kept in `kept_in`; removing `removed_by` would remove it. */
export type KeeperConflict = { path?: string; conflict?: { kept_in: { id: string; name: { path: string } }; removed_by: { id: string; name: { path: string } } } }

/**
 * How to settle a plan's keeper conflicts by taking groups out of the list.
 * The copy the user chose to keep wins: the group that would remove it goes
 * (`drop`). Two listed groups that each remove the other's kept copy are a
 * choice for the user (`choices`), unless one of them goes anyway. Groups
 * not in the list cannot be taken out: nothing here settles their conflict.
 */
export function conflictResolution(errors: readonly KeeperConflict[], listed: ReadonlySet<string>) {
  const named = errors.flatMap(error => error.conflict ? [error.conflict] : [])
  const group = (g: { id: string; name: { path: string } }): ConflictGroup => ({ id: g.id, name: g.name.path })
  const mutual = (a: string, b: string) => named.some(c => c.removed_by.id === a && c.kept_in.id === b)
    && named.some(c => c.removed_by.id === b && c.kept_in.id === a)
  const drop = new Map<string, ConflictGroup>()
  const pairs: [ConflictGroup, ConflictGroup][] = []
  for (const c of named) {
    const removing = c.removed_by.id
    const kept = c.kept_in.id
    if (!listed.has(removing)) continue
    if (listed.has(kept) && mutual(removing, kept)) {
      if (!pairs.some(([a, b]) => (a.id === removing && b.id === kept) || (a.id === kept && b.id === removing))) {
        pairs.push([group(c.kept_in), group(c.removed_by)])
      }
    } else if (!drop.has(removing)) {
      drop.set(removing, group(c.removed_by))
    }
  }
  const choices = pairs.filter(([a, b]) => !drop.has(a.id) && !drop.has(b.id))
  return { drop: [...drop.values()], choices }
}
