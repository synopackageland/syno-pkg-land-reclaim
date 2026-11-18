// Generated from schemas/v1/watch-free.schema.json by `pnpm gen:types`. Do not edit.

/**
 * Read-only, bounded observations of Btrfs data space and cleaner backlog (SPEC 5.19). One sample per interval; deltas are null with fewer than two samples and may be affected by concurrent writers.
 */
export type SynoreclaimWatchFreeResultV1 = Envelope & {
  command: 'watch-free'
  volume: Path
  duration_seconds: number
  observed_duration_ms: number
  /**
   * A stop request ended the observation before its duration.
   */
  interrupted: boolean
  interval_seconds: number
  samples: Sample[]
  /**
   * Last minus first data_used_bytes; null with fewer than two known samples.
   */
  data_used_delta_bytes: number | null
  /**
   * Last minus first free_bytes; null with fewer than two known samples.
   */
  free_delta_bytes: number | null
  /**
   * Last minus first pending_deletion_roots; null with fewer than two known samples.
   */
  pending_deletion_roots_delta: number | null
  concurrent_writers_may_affect_delta: boolean
}
/**
 * A per-target failure.
 */
export type TargetError = Path & {
  code: string
  message: string
  /**
   * busy only: who holds the filesystem or NAS-wide rootfs writer lock, when its lock file names it. command is scan, cache-clear, cleanup-apply, rootfs-apply or doctor; job_id is the writer job, null for foreground commands.
   */
  holder?: {
    job_id: string | null
    command: string
  }
  /**
   * destination_full only: the shared folder quota, rather than volume free space, is the limiting resource.
   */
  limit?: 'share_quota'
  /**
   * protected_target only: why the path is protected. share_root is a DSM shared folder itself; outside_share a path in no DSM shared folder (no Recycle Bin to move it into); package a package's data, named by package.
   */
  reason?: 'volume' | 'system' | 'recycle_bin' | 'package' | 'dsm_folder' | 'share_root' | 'home' | 'outside_share'
  /**
   * protected_target with reason package: the package whose data it is.
   */
  package?: string
  /**
   * cleanup plan --duplicates: the selected duplicate group whose copy this path is; taking that group out of the selection drops the path.
   */
  group?: string
  /**
   * keeper_conflict only: the copy at path is kept by the user in kept_in, and removing the selected group removed_by would remove it.
   */
  conflict?: {
    kept_in: GroupName
    removed_by: GroupName
  }
}
/**
 * The scan command for a volume target no report covers (SPEC 5.1).
 */
export type ScanHint = Path & {
  /**
   * The exact command to run, shell-quoted.
   */
  command: string
  last_scan: null | {
    /**
     * RFC 3339 UTC timestamp, second precision.
     */
    finished: string
    elapsed_ms: number
    entries: number
  }
}

/**
 * Fields every document starts with.
 */
export interface Envelope {
  schema_version: 1
  command: string
  tool_version: string
  /**
   * RFC 3339 UTC timestamp, second precision.
   */
  generated_at: string
  /**
   * How the command or unit ended. Exit codes: complete 0, failed 1, partial 3, busy 4, paused 10/129/130/143, runtime_limit, resource_limit and cancelled 10. cancelled: a cancel request stopped the scan; its job ended.
   */
  completion: 'complete' | 'partial' | 'busy' | 'paused' | 'runtime_limit' | 'resource_limit' | 'cancelled' | 'failed'
  job_id: string | null
  errors: TargetError[]
  warnings: Notice[]
  /**
   * Present when volume targets had no covering report: what to run for each and what its last completed volume scan took. Absent when empty.
   */
  scan_hints?: ScanHint[]
}
/**
 * A filesystem path.
 */
export interface Path {
  /**
   * The path as UTF-8; lossy when path_hex is present.
   */
  path: string
  /**
   * Exact path bytes in lowercase hex; present only when the path is not valid UTF-8.
   */
  path_hex?: string
}
/**
 * A duplicate group by id and name.
 */
export interface GroupName {
  id: string
  name: Path
}
/**
 * A warning or explanation; code is a stable reason code.
 */
export interface Notice {
  code: string
  message: string
}
export interface Sample {
  elapsed_ms: number
  /**
   * Btrfs data block group used bytes.
   */
  data_used_bytes: number | null
  /**
   * Why data_used_bytes is unknown; absent when it is known.
   */
  data_used_error?: string
  /**
   * statfs free bytes.
   */
  free_bytes: number | null
  /**
   * Why free_bytes is unknown; absent when it is known.
   */
  free_error?: string
  /**
   * Subvolumes awaiting the Btrfs cleaner.
   */
  pending_deletion_roots: number | null
  /**
   * Why pending_deletion_roots is unknown; absent when it is known.
   */
  pending_deletion_error?: string
}
