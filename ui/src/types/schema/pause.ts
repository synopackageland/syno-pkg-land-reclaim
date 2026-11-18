// Generated from schemas/v1/pause.schema.json by `pnpm gen:types`. Do not edit.

export type SynoreclaimPauseResultV1 = Envelope &
  JobRequest & {
    command: 'pause'
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
 * The answer to pause / cancel / mode --json, and the refusal of resume --json and jobs --result --json (SPEC 22.1). envelope.job_id is the job asked about. Exit 0 when requested or cancelled; a refusal exits 4 for busy (the job is running) and job_incompatible or checkpoint_unusable, 1 otherwise.
 */
export interface JobRequest {
  /**
   * requested: the running job will stop at its next safe point (a request is not proof it stopped; poll jobs). cancelled: a stopped job ended at once. too_late: the job is publishing and can no longer stop. not_running: pause of a job that is not running. finished: the job has ended. no_job: no such job. busy: resume of a job running in another process. job_incompatible: resume of a job another binary (tool version, report or cache format) started; start a new scan. not_started: resume of a queued job, which `scan --job-id` runs. no_result: jobs --result of a job with no finished run yet (command job-result). not_pausable: pause of a cleanup apply job (cancel stops it after the current entry). not_resumable: resume of a cleanup apply job (run cleanup apply with its plan again). not_adjustable: mode of a cleanup or rootfs apply job, which keeps the profile it started with. For mode, requested means the running scan goes on in the new profile from its next check, keeping its progress. checkpoint_unusable: the saved scan checkpoint is missing, damaged, incompatible or its volume/subvolume identity changed; the paused job is retained and the message explains why a fresh scan is needed.
   */
  outcome:
    | 'requested'
    | 'cancelled'
    | 'too_late'
    | 'not_running'
    | 'finished'
    | 'no_job'
    | 'busy'
    | 'job_incompatible'
    | 'not_started'
    | 'no_result'
    | 'not_pausable'
    | 'not_resumable'
    | 'not_adjustable'
    | 'checkpoint_unusable'
  /**
   * Human text; never parse it.
   */
  message: string
  /**
   * The job's state after the request, as jobs shows it; null when there is no such job.
   */
  state:
    | null
    | (
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
      )
}
