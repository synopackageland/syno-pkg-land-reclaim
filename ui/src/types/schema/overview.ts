// Generated from schemas/v1/overview.schema.json by `pnpm gen:types`. Do not edit.

export type SynoreclaimOverviewResultV1 = Envelope & {
  command: 'overview'
  results: {
    report: ReportInfo
    filesystem: {
      /**
       * Btrfs filesystem UUID.
       */
      uuid: string
      mount: Path
      /**
       * Mounted data filesystem; the system partition is handled separately.
       */
      kind: 'btrfs' | 'ext4'
      capabilities: Capabilities
    }
    space: {
      df: {
        total_bytes: number
        used_bytes: number
        free_bytes: number
        available_bytes: number
      } | null
      space_info:
        | {
            type: 'data' | 'metadata' | 'system' | 'unknown'
            profile: string
            copies: number | null
            total_bytes: number
            used_bytes: number
            captured: 'scan_end'
            used_bytes_at_scan_start?: number
          }[]
        | null
      space_info_error?: string
      inode_allocated_bytes?: number
      /**
       * statvfs used minus observed inode allocations; includes metadata, journals, unlinked files and coverage gaps. Never a reclaim estimate.
       */
      unattributed_df_bytes?: number
    }
    parts: {
      kind:
        | 'pending_cleanup'
        | 'snapshot_only'
        | 'recycle_bin'
        | 'dsm_service'
        | 'system_other'
        | 'share_live'
        | 'shared_between_folders'
        | 'pinned_unreferenced'
        | 'unknown'
      bytes: number
      reclaimable_bytes: number | null
      held_bytes: number | null
      items: {
        label?: string
        share?: string
        share_hex?: string
        service?: string
        managed_by?: string
        manage_in?: string
        /**
         * Stable snake_case id of the DSM place (SPEC 4.6).
         */
        manage_in_id?: string
        bytes: number
        reclaimable_bytes: number | null
      }[]
      reasons?: {
        code:
          | 'missing_owner'
          | 'incomplete_references'
          | 'direct_owner_unresolved'
          | 'shared_tree_owner_unresolved'
          | 'relocation_tree'
          | 'coverage_gap'
          | 'owner_budget_exhausted'
          | 'pinned_ranges_unknown'
        bytes: number
        subcases?: {
          code:
            | 'root_not_discovered'
            | 'readonly_root_parent_uuid_unmapped'
            | 'writable_root_inode_unindexed'
            | 'indexed_path_unmapped'
            | 'indexed_owner_conflict'
            | 'no_scanned_use'
            | 'shared_reference'
            | 'range_unmappable'
            | 'read_budget_exhausted'
            | 'leaf_unverified'
            | 'extent_changed'
            | 'root_unsearchable'
          bytes: number
          roots?: {
            root_id: number
            kind: 'not_discovered' | 'read_only' | 'writable'
            class:
              | 'not_discovered'
              | 'snapshot'
              | 'active_backup'
              | 'iscsi'
              | 'shared_folder'
              | 'at_directory'
              | 'hash_directory'
              | 'other_service'
              | 'other'
            at_directory: boolean
            bytes: number
          }[]
        }[]
      }[]
    }[]
    reconciliation: {
      data: {
        parts_sum_bytes: number
        space_info_used_bytes: number | null
        residual_bytes: number | null
        tolerance_bytes: number | null
        status: 'reconciled' | 'not_reconciled' | 'unknown' | 'inode_allocations'
        coverage_gaps: boolean
      }
      df: {
        used_bytes: number | null
        metadata_system_bytes: number | null
        data_metadata_system_bytes: number | null
        residual_bytes: number | null
        status: 'display_only' | 'unknown'
      }
    }
    actions: {
      rank: number
      kind: 'cleanup' | 'recycle' | 'snapshot' | 'package' | 'wait'
      share?: string
      share_hex?: string
      service?: string
      reclaimable_bytes: number | null
      held_bytes: number | null
      manage_in?: string
      /**
       * Stable snake_case id of the DSM place (SPEC 4.6).
       */
      manage_in_id?: string
      path?: Path
      /**
       * What path is, as top.list names entry kinds (file, dir); cleanup actions only.
       */
      path_kind?: string
      command?: string[]
    }[]
    attribution?: {
      read_budget: ReadBudget | null
      deferred_bytes: number
      analysis: AttributionMeasurement | null
    }
  }[]
  scans: FilesystemScan[]
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
export interface ReadBudget {
  adaptive: boolean
  used_bytes: number
  extent_count: number
  snapshot_count: number
  required_reads: number
  selected_reads: number
}
export interface AttributionMeasurement {
  elapsed_ms: number
  unknown_before: number
  unknown_after: number
  processed_extents: number
  changed_extents: number
  reads: number
  peak_rss_bytes: number | null
  plan: ReadBudget | null
}
/**
 * One filesystem of a scan.
 */
export interface FilesystemScan {
  /**
   * Btrfs filesystem UUID.
   */
  fsid: string
  mount: Path
  /**
   * How the command or unit ended. Exit codes: complete 0, failed 1, partial 3, busy 4, paused 10/129/130/143, runtime_limit, resource_limit and cancelled 10. cancelled: a cancel request stopped the scan; its job ended.
   */
  completion: 'complete' | 'partial' | 'busy' | 'paused' | 'runtime_limit' | 'resource_limit' | 'cancelled' | 'failed'
  targets: (Path & {
    scope: 'volume' | 'path'
    tree: number | null
    ino: number
    /**
     * ext4 st_dev; tree is null on ext4.
     */
    inode_device?: number
  })[]
  covered: (Path & {
    covered_by: Path
  })[]
  consistency: null | ('observed-stable' | 'changed-during-scan' | 'partial')
  change_reasons?: ChangeReasons
  /**
   * Generation changes left out of change_reasons: every inode the subvolume changed during the scan lies inside the tool's own state directory (SPEC 22.4).
   */
  state_dir_writes?: {
    subvolume_id: number
    generation_before: number
    generation_after: number
    /**
     * Inodes changed, all inside the state directory.
     */
    inodes: number
  }[]
  scan_started: null | string
  scan_finished: null | string
  totals: null | Metrics
  stats: null | ScanStats
  /**
   * Set when the memory budget stopped the scan (SPEC 22.4).
   */
  resource_limit: null | {
    /**
     * The tree being read: an fs tree, or the extent tree (2) while resolving references.
     */
    tree: number | null
    budget_bytes: number
    /**
     * Resident set at the check that tripped; null when an allocation failed instead.
     */
    resident_bytes: number | null
    /**
     * Growth the scan asked for; 0 when the resident set alone passed the budget.
     */
    reserved_bytes: number | null
    /**
     * resident_bytes + reserved_bytes.
     */
    projected_bytes: number | null
  }
  coverage: null | Coverage
  subvolumes: Subvolume[] | null
  report:
    | null
    | (Path & {
        bytes: number
      })
  errors: TargetError[]
  /**
   * Includes state_dir_in_selection and state_dir_on_scanned_filesystem when the state directory lies on this filesystem, state_dir_writes_ignored for each subvolume whose change came only from the state directory's own writes (listed in state_dir_writes), and state_dir_changed when a change reason is still the subvolume holding it; disk_limit when the scan stopped below the free-space reserve of the state filesystem (section 22.4); history_not_saved when the scan's cost could not be recorded for later estimates.
   */
  warnings: Notice[]
  /**
   * Mounted data filesystem; the system partition is handled separately.
   */
  filesystem: 'btrfs' | 'ext4'
  capabilities: Capabilities
  inode_walk?: {
    entries: number
    inodes: number
    elapsed_ms: number
    peak_rss_bytes: number | null
  }
}
export interface Metrics {
  /**
   * Apparent size of distinct regular-file inodes.
   */
  logical: number
  /**
   * Btrfs: distinct physical extents. ext4: distinct (dev, ino) st_blocks × 512 allocations, including directories and symlinks.
   */
  physical: number
  /**
   * Footprint bytes proven to have references outside the path: physical - reclaimable_resolved - unknown.
   */
  shared: number
  /**
   * Whole-extent bytes that deleting the path frees (SPEC 3). Null when any footprint reference is unresolved (unknown > 0): unknown, never a confident zero.
   */
  reclaimable: number | null
  /**
   * The resolved, conservative part of reclaimable; equals reclaimable when unknown is 0.
   */
  reclaimable_resolved: number
  /**
   * Footprint bytes of extents whose references could not be resolved; never counted as reclaimable.
   */
  unknown: number
  /**
   * Bytes of reclaimable extents that no file range references (bookends); included in reclaimable.
   */
  pinned_unreferenced: number | null
  extents: number | null
  inline: number | null
  entries: number
  files: number
}
export interface ScanStats {
  subvolumes_discovered: number
  subvolumes_scanned: number
  subvolumes_reused: number
  entries: number
  inodes: number
  extents: number
  items: number
  batches: number
  /**
   * Payload bytes of the metadata items read.
   */
  item_bytes: number
  /**
   * Extent-tree range searches for reference accounting.
   */
  reference_searches: number
  /**
   * Extent-tree items read for reference accounting.
   */
  reference_items: number
  /**
   * Tree-block lookups made by the volume overview owner walk.
   */
  owner_lookups: number
  /**
   * Extents whose overview owner walk reached --owner-budget.
   */
  owner_budget_capped_extents: number
  /**
   * Leaves read by address to measure overview pinned bytes.
   */
  pinned_leaf_reads?: number
  /**
   * Bytes of those leaf reads.
   */
  pinned_leaf_read_bytes?: number
  /**
   * Leaf reads a remembered leaf saved.
   */
  pinned_leaf_cache_hits?: number
  /**
   * Root searches for the direct references of unscanned roots.
   */
  pinned_root_searches?: number
  /**
   * Why leaves could not be read by address; absent when they could.
   */
  pinned_leaf_reader_unavailable?: string
  elapsed_ms: number
  peak_rss_bytes: number | null
  pinned_read_plan?: ReadBudget
}
export interface Subvolume {
  id: number
  role: 'scanned' | 'snapshot-skipped' | 'reused'
  generation_before: number
  /**
   * Null when the subvolume vanished during the scan.
   */
  generation_after: number | null
  /**
   * Unix seconds when a reused unit's namespace was read.
   */
  captured?: number
  /**
   * ROOT_ITEM.uuid (32 lowercase hex digits) at the final discovery, when it has one.
   */
  uuid?: string
  /**
   * ROOT_ITEM.last_snapshot at the final discovery.
   */
  last_snapshot?: number
  /**
   * Subvolumes whose parent_uuid names this one at the final discovery.
   */
  snapshots?: number[]
}
