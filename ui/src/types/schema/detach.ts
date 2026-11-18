// Generated from schemas/v1/detach.schema.json by `pnpm gen:types`. Do not edit.

/**
 * The one JSON line scan --detach prints and flushes before it returns (SPEC 5.3). completion complete: job_id names a queued job; failed (exit 1): a target did not resolve, see errors; busy (exit 4): every filesystem is locked by another writer, see errors[].holder. No job is kept unless job_id is set. resume --detach and cleanup apply --detach print the same line (SPEC 5.11, 5.7); a refused cleanup apply prints the cleanup-apply failure document instead. NAS-wide admission refuses a second queued/running scan with exit 4, completion busy, outcome scan_in_progress and running_job_id; job_id remains null and run is empty.
 */
export type SynoreclaimScanDetachResultV1 = Envelope & {
  command: 'detach'
  /**
   * self: synoreclaim started the job in a new session; none: the caller runs it, e.g. under a service manager.
   */
  launcher: 'self' | 'none'
  /**
   * The argv that runs the queued job in the foreground (scan --job-id ID --state-dir DIR; cleanup apply --yes --json ... --job-id ID ... -- PLAN); empty when no job was queued.
   */
  run: string[]
  /**
   * jobs/<job_id>/result.json, where the run leaves its whole document (scan, or cleanup-apply); null when no job was queued.
   */
  result: null | Path
  /**
   * jobs/<job_id>/output.log, the stdout and stderr of the job's own process with launcher self; null otherwise.
   */
  log: null | Path
  /**
   * scan_in_progress: a scan already occupies NAS-wide admission; no new job is queued. joined (exit 0, rootfs.report only): a walk of / is already queued or running; job_id and running_job_id name it, run is empty and nothing new starts.
   */
  outcome?: 'scan_in_progress' | 'joined'
  /**
   * The existing queued/running job to follow when outcome is set.
   */
  running_job_id?: string
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
