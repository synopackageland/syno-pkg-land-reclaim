// Generated from schemas/v1/volumes.schema.json by `pnpm gen:types`. Do not edit.

export type SynoreclaimVolumesResultV1 = Envelope & {
  command: 'volumes'
  volumes: {
    mount: Path
    fsid: null | string
    total_bytes: number | null
    used_bytes: number | null
    free_bytes: number | null
    metadata_access: boolean
    reports: number
    /**
     * Reports of this filesystem that cannot be read: damaged, or from another report format. Non-zero adds the note report_incompatible; answers that find no readable covering report then fail with the target error report_incompatible instead of no_report.
     */
    incompatible_reports: number
    last_scan: null | string
    last_consistency: null | ('observed-stable' | 'changed-during-scan' | 'partial')
    /**
     * Deleted subvolumes the Btrfs cleaner has not freed yet; their space is freed later without any action. null when subvolume discovery failed (see notes).
     */
    pending_deletion: null | {
      count: number
      /**
       * Not known before the cleaner frees it: a root item counts only its tree blocks.
       */
      bytes: null
    }
    /**
     * Btrfs writable share subvolumes, or ext4 directories verified against the trusted DSM share inventory and pinned inode identity. Ordinary ext4 volume directories are not shares.
     */
    shares: {
      name: Path
      path: Path
      subvolume_id: number | null
      /**
       * Physical bytes of the share's live namespace in the newest volume report's overview; omitted when no volume report exists.
       */
      size_bytes?: number
      inode?: number
    }[]
    notes: Notice[]
    last_attribution?: Attribution
    /**
     * Mounted data filesystem; the system partition is handled separately.
     */
    filesystem: 'btrfs' | 'ext4'
    capabilities: Capabilities
  }[]
  /**
   * The root filesystem, DSM's own partition (md0, ext4), listed apart from the data volumes; synoreclaim neither scans nor cleans it. Null when it cannot be read.
   */
  system_partition?: null | {
    mount: string
    source: string
    fstype: string
    total_bytes: number
    available_bytes: number
    used_bytes: number
    /**
     * Available bytes reported by the filesystem, also present as available_bytes for compatibility.
     */
    free_bytes: number
    /**
     * Whole percent used, rounded down.
     */
    used_percent: number
    level: 'ok' | 'getting_full' | 'full'
    level_reasons: ('used_over_recommended' | 'update_headroom_low' | 'update_may_fail' | 'nearly_full')[]
    /**
     * Less than 5 % of it is available; a system_partition_nearly_full warning says so.
     */
    nearly_full: boolean
  }
  excluded: TargetError1[]
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
 * A per-target failure.
 */
export type TargetError1 = Path & {
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
/**
 * Display-only volume attribution. Does not alter completion or cleanup certification. Absent for non-volume and legacy results.
 */
export interface Attribution {
  unknown_bytes: number
  used_bytes: number | null
  /**
   * Scan/namespace gaps, including uncertain namespace roots; unresolved references alone do not set it.
   */
  coverage_gaps: boolean
  /**
   * Present only when dangling entries in trees whose generation changed during the scan are the whole coverage gap. Display-only; absent on legacy answers means unknown.
   */
  namespace_churn?: {
    dangling_entries: number
  }
  approximation?: Approximation
}
/**
 * Completed best-effort scan accuracy; never cleanup authority. Unaccounted bytes exclude unknown sizes of skipped namespaces.
 */
export interface Approximation {
  changed_subvolumes: number
  skipped_roots: number
  unaccounted_bytes: number
}
export interface Capabilities {
  scan: boolean
  snapshots: boolean
  reflinks: boolean
  cleanup: boolean
  /**
   * Per-volume refusal code; null when cleanup is supported.
   */
  cleanup_unavailable_reason: string | null
}
