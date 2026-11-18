// A saved report's consistency (SPEC §3 "changed-during-scan"): whether DSM's
// own writes were the only thing that changed during the scan, so cleanup in
// shared folders is not affected. Shared by every page that shows a report
// chip or callout.

/** A change reason, as far as "which subvolumes changed" needs it. */
export type ChangeReason = { kind: string; subvolume_id?: number; snapshot_of?: number }

/** The consistency fields every report header carries (overview, top, recycle, snapshots). */
export type ConsistentReport = {
  consistency: string | null
  change_reasons?: readonly ChangeReason[] | null
}

/** What a share needs to be matched against a change reason. */
export type ShareIdentity = { subvolume_id: number | null }

/**
 * The subvolume ids a change reason says changed, or null when the reason
 * cannot answer "is this a shared folder?" from the data the UI has: a balance
 * (maintenance-running, maintenance-probe-failed) or a relocation root is not a
 * subvolume change, and a vanished subvolume cannot be matched against the
 * shares that still exist.
 */
function changedSubvolumes(reason: ChangeReason): number[] | null {
  switch (reason.kind) {
    case 'subvolume-generation-changed':
    case 'subvolume-snapshotted':
    case 'subvolume-uuid-changed':
      return reason.subvolume_id == null ? null : [reason.subvolume_id]
    case 'subvolume-appeared':
      // A snapshot of a shared folder is shared-folder activity, not a DSM write.
      if (reason.subvolume_id == null) return null
      return reason.snapshot_of == null ? [reason.subvolume_id] : [reason.subvolume_id, reason.snapshot_of]
    default:
      return null
  }
}

/**
 * True when a changed-during-scan report changed only outside the volume's
 * shared folders: DSM's own writes in the volume's top subvolume or another
 * system subvolume, which do not affect cleanup in shared folders.
 *
 * False whenever a change cannot be attributed this way: a shared folder's
 * subvolume changed, a reason does not name a subvolume (a balance), or a
 * vanished subvolume's share membership can no longer be read.
 */
export function changesOnlyInSystemAreas(report: ConsistentReport, shares: readonly ShareIdentity[]): boolean {
  if (report.consistency !== 'changed-during-scan') return false
  const reasons = report.change_reasons ?? []
  if (!reasons.length) return false
  const changed = new Set<number>()
  for (const reason of reasons) {
    const subvolumes = changedSubvolumes(reason)
    if (!subvolumes) return false
    for (const id of subvolumes) changed.add(id)
  }
  const shareIds = new Set(shares.map(s => s.subvolume_id))
  return [...changed].every(id => !shareIds.has(id))
}
