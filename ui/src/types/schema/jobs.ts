// Generated from schemas/v1/jobs.schema.json by `pnpm gen:types`. Do not edit.

export type SynoreclaimJobsResultV1 = Envelope & {
  command: 'jobs'
  /**
   * Newest first.
   */
  jobs: {
    job_id: string
    /**
     * What the job runs: scan, planning, system report or apply. Non-scan jobs have no scan scope, units or scan progress.
     */
    kind: 'scan' | 'cleanup-apply' | 'rootfs-apply' | 'cleanup-plan' | 'rootfs-plan' | 'rootfs-report'
    /**
     * SPEC 22.1. queued: created by scan --detach, waiting for its launcher (scan --job-id); pausing / cancelling: a running job asked to pause / cancel; interrupted: its process died while running.
     */
    state:
      | 'queued'
      | 'running'
      | 'pausing'
      | 'cancelling'
      | 'paused'
      | 'completed'
      | 'partial'
      | 'failed'
      | 'cancelled'
      | 'interrupted'
    /**
     * A scan's phase; a cleanup apply job's is execute.
     */
    phase: 'discover' | 'scan' | 'resolve-refs' | 'aggregate' | 'publish' | 'execute' | 'plan' | 'report'
    targets: Path[]
    scope: null | ('volume' | 'path')
    /**
     * The filesystems the targets resolved to in the latest run, in lock order; empty before its first run resolved them.
     */
    filesystems: {
      /**
       * Btrfs filesystem UUID.
       */
      fsid: string
      mount: Path
    }[]
    profile: 'low' | 'normal' | 'fast'
    /**
     * RFC 3339 UTC timestamp, second precision.
     */
    created: string
    /**
     * RFC 3339 UTC timestamp, second precision.
     */
    updated: string
    resumes: number
    /**
     * Units scanned or reused by the latest run.
     */
    units: number
    /**
     * Live progress of the latest run (SPEC 22.1): rewritten at most every two seconds while a scan runs, so a consumer can follow it without reading progress text. Absent when the run wrote none.
     */
    progress?: {
      /**
       * The phase the run is in, one of the scan phases.
       */
      phase: string
      /**
       * Subvolume units read or reused so far.
       */
      units_done: number
      /**
       * Writable subvolumes discovered, the most units a volume scan can have; null for a path scan.
       */
      units_total: number | null
      /**
       * Entries of the trees fully read so far plus the directory entries read so far in the tree being read, so the count moves within one large share; a reused unit does not count.
       */
      entries: number
      /**
       * Metadata items read across attempts, including rereads of changed units; checkpoint replay is not counted as fresh I/O.
       */
      items: number
      /**
       * Metadata bytes read across attempts, including changed-unit rereads; saved checkpoint replay adds no new bytes.
       */
      bytes_read: number
      /**
       * Seconds since this run started.
       */
      elapsed_s: number
      /**
       * Resident set size when the update was written.
       */
      rss_bytes: number | null
      /**
       * The subvolume being read; null between subvolumes and in phases that read none.
       */
      subvolume_id: number | null
      /**
       * The subvolume being read: its name, one entry and never a directory path (the UI shows it as the current item). Null between subvolumes and in phases that read none.
       */
      current_subvolume: null | Path
      basis: null | {
        /**
         * Entries that scan read.
         */
        entries: number
        /**
         * How long that scan took.
         */
        elapsed_s: number
        /**
         * Metadata items that scan read; null while history does not record them, so a consumer falls back to entries.
         */
        items: number | null
      }
      /**
       * RFC 3339 UTC timestamp, second precision.
       */
      updated: string
      /**
       * The changed unit being reread from its beginning; these attempt counters are separate from retained job totals.
       */
      restart?: {
        subvolume_id: number
        entries: number
        items: number
        bytes_read: number
      }
      /**
       * A planning job with several steps: a cleanup plan checks the kept copies of duplicates, then the items it moves. units_done and units_total count the current step.
       */
      step?: 'kept' | 'items'
    }
    /**
     * Live progress of a cleanup-apply job: the items moved into their Recycle Bins so far (SPEC 5.7).
     */
    cleanup?: {
      plan_id: string
      /**
       * Selected items in the plan.
       */
      items_total: number
      /**
       * Items with a result.
       */
      items_done: number
      /**
       * Items now in their shared folder's Recycle Bin.
       */
      moved: number
      /**
       * Items with no Recycle Bin, removed for good; absent when none.
       */
      deleted?: number
      /**
       * Items already gone.
       */
      already_absent: number
      /**
       * Items of which some stayed in place.
       */
      partial: number
      /**
       * Items replaced since the plan, left alone.
       */
      changed: number
      /**
       * Items DSM's tool could not move.
       */
      failed: number
      /**
       * Allocated size (st_blocks x 512) of the planned items. Moving them frees nothing until the Recycle Bin is emptied.
       */
      bytes_total: number
      /**
       * The same, over what moved.
       */
      bytes_moved: number
      /**
       * Seconds since the first move started.
       */
      elapsed_s: number
      /**
       * RFC 3339 UTC timestamp, second precision.
       */
      updated: string
      /**
       * Runs of DSM's synorecycle so far: a large folder moves child by child.
       */
      calls_done: number
    }
    /**
     * Live progress of a rootfs apply job (SPEC 5.23), rewritten at most every two seconds while the executor runs, then once more when it ends; absent for scans and cleanup jobs.
     */
    rootfs?: {
      phase: 'moves' | 'tools'
      items_done: number
      items_total: number
      bytes_done: number
      bytes_total: number
      tool:
        | null
        | (
            | 'rotate_logs'
            | 'delete_journal'
            | 'logrotate'
            | 'restart_syslog_ng'
            | 'clean_downloaded_update'
            | 'release_update_reserve'
            | 'delete_log'
          )
    }
    /**
     * Why the latest run stopped: a completion code, or cancelled.
     */
    reason: string | null
    /**
     * The binary that started the job; empty when an older binary did.
     */
    tool_version: string
    /**
     * Whether this binary can resume the job: same tool version and report/cache formats. false: resume refuses with job_incompatible; cancel still works.
     */
    compatible: boolean
    /**
     * The request a running job has pending; null otherwise.
     */
    request: 'pause' | 'cancel' | null
    /**
     * A request that came after the latest run's last check: the run finished (completed, partial or failed) anyway.
     */
    request_ignored: 'pause' | 'cancel' | null
    /**
     * The DSM user a `webapi` call (scan.start) started the job for; null for a job started from the CLI.
     */
    requested_by: string | null
    /**
     * How the latest run ended; null before one has (a queued job, or one whose process died first).
     */
    result: null | {
      /**
       * How the command or unit ended. Exit codes: complete 0, failed 1, partial 3, busy 4, paused 10/129/130/143, runtime_limit, resource_limit and cancelled 10. cancelled: a cancel request stopped the scan; its job ended.
       */
      completion:
        'complete' | 'partial' | 'busy' | 'paused' | 'runtime_limit' | 'resource_limit' | 'cancelled' | 'failed'
      /**
       * The run's exit code (SPEC 22.7).
       */
      exit_code: number
      /**
       * The reports the run published.
       */
      reports: Path[]
      /**
       * The distinct codes of the run's errors (envelope and filesystems).
       */
      error_codes: string[]
      document: Path1
    }
    /**
     * The original confirmed rootfs plan and destination, retained before queueing so interrupted applies can be followed without a final result.
     */
    rootfs_plan?: {
      plan_id: string
      /**
       * Omitted in legacy records: standard.
       */
      mode?: 'standard' | 'deep'
      move_dir: Path
    }
    /**
     * The sealed cleanup/rootfs apply plan id, including queued jobs. Absent for scans and older manifests.
     */
    plan_id?: string
    /**
     * Elapsed wall seconds of an asynchronous planning or system report job; no percentage or ETA is implied.
     */
    elapsed_s?: number
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
