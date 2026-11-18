// Generated from schemas/v1/rootfs.schema.json by `pnpm gen:types`. Do not edit.

/**
 * What fills DSM's system partition `/` (md0), read-only: df-style space and inodes, the /var/log quota, a bounded, paced walk of `/` alone (never other mounts), what `rootfs plan` could move off it, what it never selects and why, and deleted files still held open. Or a refusal: completion failed, errors[0].code not_root.
 */
export type SynoreclaimRootfsReportV1 = RootfsReportSuccess | RootfsReportRefusal
export type RootfsReportSuccess = Envelope &
  RootfsReportFields & {
    command: 'rootfs'
    completion: 'complete' | 'partial'
    filesystem: Filesystem
    /**
     * DSM's project quota on /var/log; null when it has none.
     */
    log_quota: null | {
      path: '/var/log'
      /**
       * The ext4 project of /var/log.
       */
      project_id: number
      used_bytes: number
      soft_limit_bytes: number
      hard_limit_bytes: number
    }
    walk: Walk
    usage: Usage
    owners: Owners
    /**
     * The owners under below_bytes (1 MiB), folded out of owners into one row; null when there are none.
     */
    owners_small: null | {
      below_bytes: number
      items: number
      bytes: number
    }
    cleanable: Cleanable
    report_only: ReportOnly
    /**
     * The report_only under below_bytes (1 MiB), folded out of report_only into one row; null when there are none.
     */
    report_only_small: null | {
      below_bytes: number
      items: number
      bytes: number
    }
    open_deleted: OpenDeleted
    not_walked: NotWalked
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
export type UsageRow = UsageEntry & {
  /**
   * The largest second-level paths below it, at most 10.
   */
  children: UsageEntry[]
  /**
   * Bytes of the second-level paths children leaves out.
   */
  other_children_bytes: number
}
export type UsageEntry = Path & {
  bytes: number
  /**
   * Share of the partition's used bytes, one decimal.
   */
  percent_of_used: number
  category: Category
  /**
   * The package the path belongs to, when it is package data.
   */
  package: string | null
}
/**
 * Who or what owns a path on `/`.
 */
export type Category =
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
/**
 * First-level paths of `/`, largest first.
 */
export type Usage = UsageRow[]
/**
 * Bytes by owner, largest first.
 */
export type Owners = {
  category: Category
  /**
   * The package, or /root or /usr/local for user_tooling.
   */
  name: string | null
  /**
   * The package's name in Package Center, from its INFO displayname; null when unknown or not package data. Reports before 0.1.1-1600 omit it.
   */
  display_name?: string | null
  bytes: number
  /**
   * Share of the partition's used bytes, one decimal.
   */
  percent_of_used: number
}[]
export type ByCategory = {
  category: Category
  bytes: number
  items: number
}[]
/**
 * Owners rootfs plan never selects, with what to do instead.
 */
export type ReportOnly = {
  category: Category
  name: string | null
  bytes: number
  advice: ReportOnlyAdvice
}[]
export type NotWalked = (Path & {
  reason: 'mount_point' | 'unreadable' | 'too_deep'
})[]
/**
 * A refusal: no report; errors[0].code says why.
 */
export type RootfsReportRefusal = Envelope &
  RootfsReportFields & {
    completion?: 'failed'
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
export interface RootfsReportFields {
  command?: 'rootfs'
  filesystem?: Filesystem
  /**
   * DSM's project quota on /var/log; null when it has none.
   */
  log_quota?: null | {
    path: '/var/log'
    /**
     * The ext4 project of /var/log.
     */
    project_id: number
    used_bytes: number
    soft_limit_bytes: number
    hard_limit_bytes: number
  }
  walk?: Walk
  usage?: Usage
  owners?: Owners
  /**
   * The owners under below_bytes (1 MiB), folded out of owners into one row; null when there are none.
   */
  owners_small?: null | {
    below_bytes: number
    items: number
    bytes: number
  }
  cleanable?: Cleanable
  report_only?: ReportOnly
  /**
   * The report_only under below_bytes (1 MiB), folded out of report_only into one row; null when there are none.
   */
  report_only_small?: null | {
    below_bytes: number
    items: number
    bytes: number
  }
  open_deleted?: OpenDeleted
  not_walked?: NotWalked
}
export interface Filesystem {
  mount: '/'
  source: string
  fstype: string
  total_bytes: number
  /**
   * total_bytes - free_bytes, as df counts it.
   */
  used_bytes: number
  free_bytes: number
  /**
   * Free bytes an unprivileged writer may use.
   */
  available_bytes: number
  /**
   * df's Use%: used / (used + available), rounded up.
   */
  used_percent: number
  /**
   * The shared T1–T4 system-partition level from the space available, used percentage, and total size.
   */
  level?: 'ok' | 'getting_full' | 'full'
  /**
   * Every shared T1–T4 reason that applies to the system partition.
   */
  level_reasons?: ('used_over_recommended' | 'update_headroom_low' | 'update_may_fail' | 'nearly_full')[]
  inodes_total: number
  inodes_used: number
  /**
   * Available space is below 5% of the total.
   */
  nearly_full: boolean
}
export interface Walk {
  entries: number
  /**
   * st_blocks bytes of distinct inodes walked.
   */
  allocated_bytes: number
  complete: boolean
  stop_reason: null | 'entry_limit' | 'paused' | 'cancelled' | 'runtime_limit' | 'resource_limit' | 'disk_limit'
  unreadable_dirs: number
  elapsed_ms: number
}
/**
 * An estimate of what `rootfs plan --to <shared folder>` could move off `/`: every allowlisted entry the walk found. A plan may move less: it leaves files a process holds open (its held_open), and by size or age (its kept) /var/log *.xz archives modified in the last 7 days, /.log.junior logs smaller than 1 MiB, update leftovers other than junior logs modified or changed since the last boot, and files larger than 8 GiB. apply may move less again: it leaves what changed since the plan or a process opened since.
 */
export interface Cleanable {
  bytes: number
  /**
   * cleanable.bytes plus the 256 MiB margin rootfs plan keeps free.
   */
  required_destination_bytes: number
  items: number
  by_category: ByCategory
  /**
   * DSM's fixed tools that rootfs plan would run on this host.
   */
  tools: (
    | 'rotate_logs'
    | 'delete_journal'
    | 'logrotate'
    | 'restart_syslog_ng'
    | 'clean_downloaded_update'
    | 'release_update_reserve'
    | 'delete_log'
  )[]
  /**
   * What rootfs plan --mode deep could move, Standard's items included, after the same limits; Hyper Backup's restore installer only while / would lack a DSM update's room without it. Absent from reports cached by an older synoreclaim.
   */
  deep?: {
    bytes: number
    required_destination_bytes: number
    items: number
    by_category: ByCategory
    /**
     * deep.bytes beyond cleanable.bytes.
     */
    additional_bytes: number
    /**
     * DSM's fixed tools that a deep plan would run on this host.
     */
    tools: (
      | 'rotate_logs'
      | 'delete_journal'
      | 'logrotate'
      | 'restart_syslog_ng'
      | 'clean_downloaded_update'
      | 'release_update_reserve'
      | 'delete_log'
    )[]
  }
}
export interface ReportOnlyAdvice {
  /**
   * A stable advice key for an owner that rootfs plan leaves in place.
   */
  code: 'user_tooling' | 'package_data' | 'live_logs' | 'log_center_db' | 'geoip_database' | 'own_state' | 'dsm_keep'
  /**
   * The UI destination for managing this owner, when one applies.
   */
  manage_in_id?: 'package_center'
  message: string
}
/**
 * Deleted files on `/` that processes still hold open.
 */
export interface OpenDeleted {
  bytes_estimate: number
  holders: {
    pid: number
    comm: string
    inodes: number
    bytes_estimate: number
    advice: HolderAdvice
  }[]
  unreadable_processes: number
}
export interface HolderAdvice {
  /**
   * A stable advice key for freeing deleted files held by a process.
   */
  code: 'restart_syslog_ng' | 'restart_holder'
  message: string
}
