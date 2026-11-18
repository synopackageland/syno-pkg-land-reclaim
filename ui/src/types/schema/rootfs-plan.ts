// Generated from schemas/v1/rootfs-plan.schema.json by `pnpm gen:types`. Do not edit.

/**
 * A plan to move allowlisted leftovers off `/` into a folder in a shared folder on a data volume. The plan is written as <plan_id>.json into root's private /var/lib/synoreclaim/rootfs-plans (never into the share, where others could edit it) for `rootfs apply`; apply later moves the items into <folder>/SynoReclaim-system-YYYYMMDD-HHMM and records the plan id in the move folder manifest. README.txt lists the original paths. Nothing else on `/` changes. With nothing to move and no tool to run, plan_file, plan_id, expires_at and apply are null and no file is written. A refusal has completion failed and errors[0].code, one of: not_root, dsm_update_active, destination_not_directory, destination_not_in_share, destination_on_system_partition, destination_protected, destination_full, destination_quota_unavailable, plan_dir_untrusted, walk_stopped, processes_unreadable.
 */
export type SynoreclaimRootfsPlanResultV1 = RootfsPlanSuccess | RootfsPlanRefusal | RootfsPlanBusy
export type RootfsPlanSuccess = Envelope &
  RootfsPlanFields & {
    command: 'rootfs-plan'
    completion: 'complete'
    plan_file: null | Path
    plan_id: string | null
    expires_at: null | string
    destination: Destination
    root: Root
    moves: Moves
    items: Items
    held_open: HeldOpen
    kept: Kept
    tools: Tools
    apply: null | {
      command: string[]
      notice: Notice
      tools_notice: null | Notice
    }
    follow_up?: FollowUp
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
 * The --to folder.
 */
export type Destination = Path & {
  share: Path
  move_dir: Path
  available_bytes: number
  /**
   * The moved bytes plus the margin apply keeps free.
   */
  required_bytes: number
}
/**
 * What apply moves, sorted by path. A folder is removed only once it is empty.
 */
export type Items = (Path & {
  kind: 'file' | 'dir'
  /**
   * Who or what owns a path on `/`.
   */
  category:
    | 'update_leftover'
    | 'log_archive'
    | 'crash_dump'
    | 'temp_debris'
    | 'logs'
    | 'log_center_db'
    | 'geoip_database'
    | 'package_data'
    | 'user_tooling'
    | 'own_state'
    | 'dsm_keep'
    | 'dsm_system'
    | 'other'
    | 'preference_image'
    | 'samba_cache'
    | 'tool_cache'
    | 'backup_installer'
  bytes: number
})[]
/**
 * Movable files a process holds open, left on /.
 */
export type HeldOpen = (Path & {
  bytes: number
  holder: string
})[]
/**
 * Movable files the plan leaves on / by their size or age, as support needs them; not in the plan.
 */
export type Kept = (Path & {
  bytes: number
  /**
   * recent_log_archive: a /var/log *.xz archive modified in the last 7 days (disk-latency and healthtest archives are exempt). small_junior_log: a /.log.junior/logs.tar.* under 1 MiB. updated_since_boot: an update leftover under /upd@te, /@autoupdate or /.SynoUpgrade* (not a junior log, which DSM renames at every boot) whose mtime or ctime is not older than the last boot; it may be an installed update waiting for the restart. too_large: a file larger than 8 GiB, more than DSM's whole system partition, so sparse. append_only: a file with chattr +a (as DSM sets on /var/log/rm.log*), which the kernel refuses to unlink. immutable: a file or folder with chattr +i, or anything below such a folder. update_room_enough: Hyper Backup's restore installer (deep), while / with the other moves has room for a typical DSM update.
   */
  reason:
    | 'recent_log_archive'
    | 'small_junior_log'
    | 'updated_since_boot'
    | 'too_large'
    | 'append_only'
    | 'immutable'
    | 'unrecognised_image'
    | 'update_room_enough'
})[]
/**
 * A DSM tool run by its fixed name; each maps to one fixed argv.
 */
export type Tool =
  | 'rotate_logs'
  | 'delete_journal'
  | 'logrotate'
  | 'restart_syslog_ng'
  | 'clean_downloaded_update'
  | 'release_update_reserve'
  | 'delete_log'
/**
 * DSM's own tools apply runs after the moves, in order: synocleaner --rotate-log and --delete-journal from DSM 6.2.3, else logrotate -f /etc/logrotate.conf; synosystemctl restart syslog-ng only while syslog-ng holds deleted files open. Each is a fixed argv run without a shell.
 */
export type Tools = {
  tool: Tool
  argv: string[]
  effect: string
  /**
   * Allocated rootfs bytes of the fixed cleanup input; other tool savings are unknown.
   */
  estimated_bytes?: number
}[]
/**
 * Manual actions; no automatic restart or reboot.
 */
export type FollowUp = (
  'refresh_geoip' | 'reset_images' | 'restart_winbind' | 'download_update' | 'update_dsm' | 'backup_installer'
)[]
/**
 * A refusal: no plan; errors[0].code says why.
 */
export type RootfsPlanRefusal = Envelope &
  RootfsPlanFields & {
    completion?: 'failed'
  }
export type RootfsPlanBusy = Envelope &
  RootfsPlanFields & {
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
export interface RootfsPlanFields {
  command?: 'rootfs-plan'
  plan_file?: null | Path
  plan_id?: string | null
  expires_at?: null | string
  destination?: Destination
  root?: Root
  /**
   * Omitted in legacy records: standard.
   */
  mode?: 'standard' | 'deep'
  /**
   * Additional allocated movable bytes versus standard, excluding DSM tools.
   */
  additional_bytes?: number
  moves?: Moves
  items?: Items
  held_open?: HeldOpen
  kept?: Kept
  tools?: Tools
  apply?: null | {
    command: string[]
    notice: Notice
    tools_notice: null | Notice
  }
  follow_up?: FollowUp
}
export interface Root {
  available_bytes: number
  /**
   * Estimated: available plus the moved items' allocated bytes.
   */
  available_after_bytes: number
}
export interface Moves {
  bytes: number
  /**
   * moves.bytes plus the 256 MiB margin apply keeps free.
   */
  required_destination_bytes: number
  items: number
  by_category: {
    /**
     * Who or what owns a path on `/`.
     */
    category:
      | 'update_leftover'
      | 'log_archive'
      | 'crash_dump'
      | 'temp_debris'
      | 'logs'
      | 'log_center_db'
      | 'geoip_database'
      | 'package_data'
      | 'user_tooling'
      | 'own_state'
      | 'dsm_keep'
      | 'dsm_system'
      | 'other'
      | 'preference_image'
      | 'samba_cache'
      | 'tool_cache'
      | 'backup_installer'
    bytes: number
    items: number
  }[]
}
