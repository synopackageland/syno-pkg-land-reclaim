import type { RootfsMoveItem } from './cleanupTypes'

export type CleanupShare = {
  name: string
  path: string
  mount: string
  freeBytes: number
}

export type ShareBlockReason = 'space' | 'busy'

type VolumeJob = {
  kind?: string
  state: string
  filesystems?: Array<{ mount: { path: string } }>
}

const ACTIVE_STATES = new Set(['queued', 'running', 'pausing', 'cancelling'])
const SMALL_FILE_BYTES = 1024 * 1024

export function scanningVolumeMounts(jobs: VolumeJob[]): Set<string> {
  return new Set(
    jobs
      .filter(job => job.kind === 'scan' && ACTIVE_STATES.has(job.state))
      .flatMap(job => job.filesystems?.map(fs => fs.mount.path) ?? []),
  )
}

export function shareReasons(share: CleanupShare, requiredBytes: number): ShareBlockReason[] {
  return [
    ...(share.freeBytes < requiredBytes ? ['space' as const] : []),
  ]
}

export function defaultDestination(
  shares: CleanupShare[],
  requiredBytes: number,
): CleanupShare | null {
  return shares
    .filter(share => shareReasons(share, requiredBytes).length === 0)
    .reduce<CleanupShare | null>((best, share) => (best === null || share.freeBytes > best.freeBytes ? share : best), null)
}

export function splitSmallMoveItems(items: RootfsMoveItem[]) {
  const visible = items.filter(item => item.bytes >= SMALL_FILE_BYTES)
  const folded = items.filter(item => item.bytes < SMALL_FILE_BYTES)
  return {
    visible,
    foldedCount: folded.length,
    foldedBytes: folded.reduce((sum, item) => sum + item.bytes, 0),
  }
}

const REFUSAL_KEYS: Record<string, string> = {
  destination_full: 'rootfsClean.error.destinationFull',
  destination_not_directory: 'rootfsClean.error.destinationNotDirectory',
  destination_not_in_share: 'rootfsClean.error.destinationNotInShare',
  destination_on_system_partition: 'rootfsClean.error.destinationOnSystemPartition',
  destination_protected: 'rootfsClean.error.destinationProtected',
  busy: 'rootfsClean.error.busy',
  apply_running: 'rootfsClean.error.rootfsBusy',
  backend_busy: 'rootfsClean.error.rootfsBusy',
  rootfs_busy: 'rootfsClean.error.rootfsBusy',
  walk_incomplete: 'rootfsClean.error.incomplete',
  walk_stopped: 'rootfsClean.error.incomplete',
  processes_unreadable: 'rootfsClean.error.incomplete',
  dsm_update_active: 'rootfsClean.error.dsmUpdate',
  plan_stale: 'rootfsClean.error.stale',
  root_changed: 'rootfsClean.error.stale',
  destination_changed: 'rootfsClean.error.stale',
  plan_invalid: 'rootfsClean.error.stale',
  plan_untrusted: 'rootfsClean.error.untrusted',
  plan_dir_untrusted: 'rootfsClean.error.untrusted',
  not_root: 'rootfsClean.error.notRoot',
}

export function refusalMessageKey(code: string | undefined, holderCommand?: string): string {
  if (code === 'busy' && holderCommand === 'rootfs-apply') return 'rootfsClean.error.rootfsBusy'
  return (code && REFUSAL_KEYS[code]) || 'rootfsClean.error.unknown'
}
