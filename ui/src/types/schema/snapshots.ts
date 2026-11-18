// Generated from schemas/v1/snapshots.schema.json by `pnpm gen:types`. Do not edit.

/**
 * Which snapshot holds the space and what deleting a set of snapshots frees (SPEC 5.14), from a scan with --snapshots. Nothing is deleted.
 */
export type SynoreclaimSnapshotsResultV1 = Envelope & {
  command: 'snapshots'
  /**
   * One result per target.
   */
  results: {
    path: string
    path_hex?: string
    report: ReportInfo
    shares: {
      share: Path1
      live_subvolume_id: number
      /**
       * Bytes whose references are all snapshots of this share.
       */
      held_only_by_snapshots_bytes: number
      /**
       * Bytes snapshots of this share may hold alone that could not be proven.
       */
      unknown_bytes: number
      /**
       * Snapshots not read (older than the --snapshots count).
       */
      snapshots_not_materialized: number
      /**
       * Newest first.
       */
      snapshots: {
        subvolume_id: number
        name: Path2
        /**
         * Creation time (ROOT_ITEM otime), RFC 3339 UTC.
         */
        otime: string | null
        read_only: boolean
        pending_deletion: boolean
        /**
         * Its file extents were read; the byte values are null otherwise.
         */
        materialized: boolean
        /**
         * Bytes only this snapshot references: what deleting it alone frees.
         */
        exclusive_bytes: number | null
        /**
         * Bytes allocated in extents freed by this snapshot that no file range references; null when ownership or range mapping is unknown.
         */
        pinned_unreferenced_bytes: number | null
        /**
         * Bytes this snapshot and the live share both reference; deleting snapshots never frees them.
         */
        shared_with_live_bytes: number | null
        /**
         * Bytes only this snapshot references among the roots read, whose other owners could not be ruled out.
         */
        unknown_bytes: number | null
        /**
         * The kernel's level-0 qgroup excl; null unless quotas are on, consistent and not rescanning.
         */
        qgroup_exclusive_bytes: number | null
      }[]
      /**
       * Deleted snapshots awaiting the Btrfs cleaner.
       */
      pending_deletion_ids: number[]
      held_files: HeldFiles
      simulation?: SnapshotSimulation
      /**
       * Where DSM deletes the snapshots.
       */
      manage_in: string
      /**
       * Stable snake_case id of that DSM place (SPEC 4.6).
       */
      manage_in_id: string
    }[]
    snapshot_analysis?: {
      started: number
      finished: number
      elapsed_ms: number
      peak_rss_bytes: number | null
      changed_live_subvolumes?: number[]
    }
  }[]
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
 * The report an answer came from; path is the report file.
 */
export type ReportInfo = Path & {
  /**
   * Btrfs filesystem UUID.
   */
  fsid: string
  job_id: string
  /**
   * RFC 3339 UTC timestamp, second precision.
   */
  scan_started: string
  /**
   * RFC 3339 UTC timestamp, second precision.
   */
  scan_finished: string
  age_seconds: number
  /**
   * Older than --max-age, or a cleanup ran since the scan (cleanup_since_report, or an unreadable cleanup record).
   */
  stale: boolean
  /**
   * observed-stable means no change was detected, not proof of an atomic view.
   */
  consistency: 'observed-stable' | 'changed-during-scan' | 'partial'
  change_reasons?: ChangeReasons
  coverage: Coverage
  /**
   * The newest cleanup applied since this report's scan, which made it stale (SPEC 5.7 "Reports after cleanup"); null when none.
   */
  cleanup_since_report?: null | {
    plan_id: string
    /**
     * The execution log's file name under the state directory's cleanup/logs.
     */
    execution_log: string
    /**
     * RFC 3339 UTC timestamp, second precision.
     */
    applied_at: string
  }
  attribution?: Attribution
  /**
   * Mounted data filesystem; the system partition is handled separately.
   */
  filesystem: 'btrfs' | 'ext4'
  capabilities: Capabilities
}
/**
 * Why consistency is changed-during-scan; empty when no change was detected.
 */
export type ChangeReasons = (
  | {
      kind: 'subvolume-generation-changed'
      subvolume_id: number
      selected: boolean
      generation_before: number
      generation_after: number
    }
  | {
      kind: 'subvolume-appeared'
      subvolume_id: number
      generation: number
      /**
       * Absent in reasons written before this field; read as false.
       */
      has_uuid?: boolean
      snapshot_of?: number
    }
  | {
      kind: 'subvolume-vanished'
      subvolume_id: number
      generation_before: number
    }
  | {
      kind: 'subvolume-snapshotted'
      subvolume_id: number
      last_snapshot_before: number
      last_snapshot_after: number
    }
  | {
      kind: 'subvolume-uuid-changed'
      subvolume_id: number
    }
  | {
      kind: 'maintenance-running'
      /**
       * A Btrfs operation that rewrites extent mappings.
       */
      operation: 'balance' | 'dev-replace'
    }
  | {
      kind: 'maintenance-probe-failed'
      /**
       * A Btrfs operation that rewrites extent mappings.
       */
      operation: 'balance' | 'dev-replace'
      errno: number | null
    }
  | {
      kind: 'relocation-tree'
      subvolume_id: number
    }
)[]
/**
 * The largest files whose extents only this share's snapshots reference; null for a report written before the list existed.
 */
export type HeldFiles = {
  /**
   * Every file with snapshot-only data was tallied (--held-files-budget).
   */
  complete: boolean
  /**
   * Files tallied; files lists the largest of them (--limit).
   */
  count: number
  files: {
    /**
     * The newest read snapshot that references the file.
     */
    snapshot_id: number
    snapshot: Path
    ino: number
    /**
     * Below the snapshot root; null when the name is unresolved.
     */
    path: Path | null
    /**
     * The same path in the live share; null when the name is unresolved.
     */
    live_path: Path | null
    /**
     * The live share still has the inode: the file was rewritten, not deleted.
     */
    in_live: boolean
    /**
     * Bytes of the file's extents that only snapshots reference, each extent once.
     */
    bytes: number
    /**
     * Bytes of the file's extents only snapshots reference among the roots read, whose other owners could not be ruled out.
     */
    unknown_bytes: number
  }[]
} & ({
  /**
   * Every file with snapshot-only data was tallied (--held-files-budget).
   */
  complete: boolean
  /**
   * Files tallied; files lists the largest of them (--limit).
   */
  count: number
  files: {
    /**
     * The newest read snapshot that references the file.
     */
    snapshot_id: number
    snapshot: Path
    ino: number
    /**
     * Below the snapshot root; null when the name is unresolved.
     */
    path: Path | null
    /**
     * The same path in the live share; null when the name is unresolved.
     */
    live_path: Path | null
    /**
     * The live share still has the inode: the file was rewritten, not deleted.
     */
    in_live: boolean
    /**
     * Bytes of the file's extents that only snapshots reference, each extent once.
     */
    bytes: number
    /**
     * Bytes of the file's extents only snapshots reference among the roots read, whose other owners could not be ruled out.
     */
    unknown_bytes: number
  }[]
} | null)

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
 * What a report does not cover; unknown is not zero.
 */
export interface Coverage {
  /**
   * Subvolumes whose namespace has gaps, encrypted entries or failed roots. Empty proves no namespace gaps; absent on legacy reports means unknown. Does not certify reference accounting.
   */
  namespace_uncertain_subvolumes?: number[] | null
  encrypted_directories: number
  snapshots_not_materialized: number | null
  snapshots_materialized?: number | null
  unlinked_subvolume_entries: number | null
  dangling_entries: number
  cycles: number
  unsupported_locations: number
  missing_subvolume_roots: number | null
  /**
   * Requested roots the scan could not read. Absent on legacy reports: unknown, not zero.
   */
  unreadable_roots?: number
  /**
   * complete: every extent's references were resolved; partial: some extents are unknown; unavailable: reports older than reference accounting.
   */
  reference_accounting: 'complete' | 'partial' | 'unavailable'
  /**
   * Extents with no extent-tree item or unreadable references.
   */
  unresolved_extents: number | null
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
/**
 * A filesystem path.
 */
export interface Path1 {
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
 * A filesystem path.
 */
export interface Path2 {
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
 * What deleting the selected snapshots together frees.
 */
export interface SnapshotSimulation {
  selector: string
  selected_ids: number[]
  /**
   * Bytes all of whose references are selected snapshots or deleted ones awaiting the cleaner: one set operation, not a sum.
   */
  freed_bytes: number
  /**
   * Bytes the set may free whose other owners could not be ruled out.
   */
  unknown_bytes: number
  /**
   * Selected snapshots whose file extents were not read; what they alone hold is not in freed_bytes.
   */
  not_materialized_ids: number[]
  /**
   * The most deleting the set may free: freed_bytes plus unknown_bytes plus external bytes a selected snapshot of another share may hold. Null when a selected snapshot was not read. freed_bytes is the least.
   */
  upper_bound_bytes: number | null
  /**
   * freed_bytes is what deleting the set frees: every selected snapshot was read and no extent it may free is unresolved or shared with another share's snapshot.
   */
  exact: boolean
  /**
   * Why exact is false; empty when it is true.
   */
  inexact_reasons: ('not_materialized' | 'unknown_owners' | 'cross_share_sharing')[]
  /**
   * The scan (--snapshots all, raised owner budgets) that reads what made the answer inexact; null when it is exact or no scan option helps (cross_share_sharing, missing roots).
   */
  exact_command: string | null
}
