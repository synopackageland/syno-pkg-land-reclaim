// The system partition's state on Overview. U1 uses
// the CLI's `level` (A1) when present and keeps the rules below for older
// volumes.list payloads.
import type { VolumesDocument } from '@/types'

export type SystemPartition = NonNullable<VolumesDocument['system_partition']>
export type SystemPartitionLevel = 'ok' | 'getting_full' | 'full'
type SystemPartitionStatus = Pick<SystemPartition, 'total_bytes' | 'available_bytes'> & {
  level?: SystemPartitionLevel
}

/** T2: a DSM update needs the patch size plus 130 MB on the system partition. */
const TYPICAL_PATCH_BYTES = 389_000_000
const UPDATE_MARGIN_BYTES = 130_000_000
/** T2 with §2's example patch size, until a real patch size is known. */
export const UPDATE_ROOM_BYTES = TYPICAL_PATCH_BYTES + UPDATE_MARGIN_BYTES
/** T3: the worst-case update room, which the CLI rounds up to 1 GiB. */
export const UPDATE_HEADROOM_BYTES = 1_073_741_824
/** T1: DSM recommends staying under 70 % used. */
export const RECOMMENDED_USED_PERCENT = 70
/** T4: less than 5 % available is nearly full. */
export const NEARLY_FULL_PERCENT = 5
/** Above this much available the partition is normal whatever T1–T4 say:
 * 900 MB as DSM counts it (900 × 1024² bytes), as the CLI does. */
export const NO_WARNING_ABOVE_BYTES = 900 * 1024 * 1024

/** New scan offers to check the partition again only while this is low (below 900 MB). */
export function systemCheckOffered(partition: Pick<SystemPartition, 'available_bytes'> | null | undefined) {
  return !!partition && partition.available_bytes < NO_WARNING_ABOVE_BYTES
}

/**
 * Normal, getting full or full — null when the partition cannot be read, so
 * the UI shows nothing. Checked in §2's order.
 */
export function systemPartitionLevel<T extends SystemPartitionStatus>(
  partition: T | null | undefined,
): SystemPartitionLevel | null {
  if (!partition) return null
  // Also over a CLI level, which a report cached by an older CLI can hold.
  if (partition.available_bytes > NO_WARNING_ABOVE_BYTES && partition.total_bytes > 0) return 'ok'
  const cliLevel = partition.level
  if (cliLevel === 'ok' || cliLevel === 'getting_full' || cliLevel === 'full') {
    return cliLevel
  }
  if (!(partition.total_bytes > 0)) return null
  const { total_bytes: total, available_bytes: available } = partition
  const updateFailsBelow = Math.max(UPDATE_ROOM_BYTES, (total * NEARLY_FULL_PERCENT) / 100)
  if (available < updateFailsBelow) return 'full'
  const used = total - available
  if (used * 100 >= total * RECOMMENDED_USED_PERCENT || available < UPDATE_HEADROOM_BYTES) return 'getting_full'
  return 'ok'
}
