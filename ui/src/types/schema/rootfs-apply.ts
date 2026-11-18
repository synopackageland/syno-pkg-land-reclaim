// Generated from schemas/v1/rootfs-apply.schema.json by `pnpm gen:types`. Do not edit.

/**
 * What `rootfs apply PLAN --yes` moved off `/` into the plan's move folder, what it left and why, and how much space `/` gained as statfs measured it. Apply writes README.txt with the original paths and records the plan id as the first manifest.jsonl line. Each moved file is copied, checked by SHA-256 and then removed from `/`; later lines in manifest.jsonl record each outcome. There is no restore command. Then the plan's DSM tools run, each checked again first; what they delete is gone for good. Completion is partial when an item was left, paused when the run stopped between items (run the same apply again to go on). A refusal has completion failed and errors[0].code, one of: not_root, dsm_update_active, yes_required, mutations_disabled, config_untrusted, plan_dir_untrusted, plan_untrusted, plan_invalid, plan_stale, root_changed, destination_changed, destination_not_in_share, destination_on_system_partition, destination_protected, destination_full, processes_unreadable, apply_running.
 */
export type SynoreclaimRootfsApplyResultV1 = RootfsApplySuccess | RootfsApplyRefusal | RootfsApplyBusy
export type RootfsApplySuccess = Envelope &
  RootfsApplyFields & {
    command: 'rootfs-apply'
    completion: 'complete' | 'partial' | 'paused' | 'cancelled' | 'runtime_limit' | 'resource_limit'
    plan_id: string
    plan_file: Manifest
    move_dir: Manifest
    manifest: Manifest
    moved: Moved
    skipped: Skipped
    root: Root
    notice: Notice
    tools: Tools
    tools_notice: null | Notice
    follow_up?: FollowUp
  }
/**
 * A per-target failure.
 */
export type TargetError = Manifest & {
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
export type ScanHint = Manifest & {
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
export type Skipped = (Manifest & {
  reason:
    | 'missing'
    | 'changed'
    | 'path_changed'
    | 'held_open'
    | 'changed_during_copy'
    | 'copy_mismatch'
    | 'copy_exists'
    | 'no_room'
    | 'not_empty'
    | 'unlink_failed'
})[]
/**
 * The planned DSM tools that ran after the moves, in order. failed carries exit_code and killed carries signal; untrusted means the program was no longer root's unwritable executable and not_needed that syslog-ng no longer held deleted files, so neither ran. A run other than succeeded or not_needed makes completion partial.
 */
export type Tools = {
  /**
   * A DSM tool run by its fixed name; each maps to one fixed argv.
   */
  tool:
    | 'rotate_logs'
    | 'delete_journal'
    | 'logrotate'
    | 'restart_syslog_ng'
    | 'clean_downloaded_update'
    | 'release_update_reserve'
    | 'delete_log'
  argv: string[]
  outcome: 'succeeded' | 'failed' | 'killed' | 'timed_out' | 'not_started' | 'untrusted' | 'not_needed'
  exit_code?: number
  signal?: number
}[]
/**
 * Manual actions; no automatic restart or reboot.
 */
export type FollowUp = (
  'refresh_geoip' | 'reset_images' | 'restart_winbind' | 'download_update' | 'update_dsm' | 'backup_installer'
)[]
/**
 * A refusal: nothing moved; errors[0].code says why.
 */
export type RootfsApplyRefusal = Envelope &
  RootfsApplyFields & {
    completion?: 'failed'
  }
export type RootfsApplyBusy = Envelope &
  RootfsApplyFields & {
    completion?: 'busy'
    errors: (TargetError & {
      code: 'busy'
    })[]
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
export interface Manifest {
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
  name: Manifest
}
/**
 * A warning or explanation; code is a stable reason code.
 */
export interface Notice {
  code: string
  message: string
}
export interface RootfsApplyFields {
  command?: 'rootfs-apply'
  plan_id?: string
  plan_file?: Manifest
  move_dir?: Manifest
  manifest?: Manifest
  /**
   * Omitted in legacy records: standard.
   */
  mode?: 'standard' | 'deep'
  moved?: Moved
  skipped?: Skipped
  root?: Root
  tools?: Tools
  notice?: Notice
  tools_notice?: null | Notice
  follow_up?: FollowUp
}
export interface Moved {
  files: number
  size_bytes: number
  removed_dirs: number
}
export interface Root {
  available_before_bytes: number
  available_after_bytes: number
  /**
   * available_after_bytes - available_before_bytes; negative when other writers on / used more than the run freed.
   */
  freed_bytes: number
}
