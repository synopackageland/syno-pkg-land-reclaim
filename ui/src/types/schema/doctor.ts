// Generated from schemas/v1/doctor.schema.json by `pnpm gen:types`. Do not edit.

/**
 * Read-only environment diagnostics. The system_partition block is the same as in volumes.list.
 */
export interface SynoreclaimDoctorResultV1 {
  schema_version: 1
  target: string
  kernel: string | null
  filesystem_uuid: string | null
  subvolume_id: number | null
  checks: {
    name: string
    status: 'pass' | 'warn' | 'fail' | 'skip'
    detail: string
  }[]
  /**
   * The root filesystem, DSM's own partition (md0, ext4), listed apart from the data volumes; synoreclaim neither scans nor cleans it. Null when it cannot be read.
   */
  system_partition: null | {
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
  subvolumes: null | {
    count: number
    readonly: number
    pending_deletion: number
    pending_deletion_bytes: number | null
    quota_enabled: boolean
  }
  item_sample: null | {
    items: number
    unknown_types: {
      [k: string]: number
    }
  }
  scheduling: null | {
    profile: 'low' | 'normal' | 'fast'
    requested_nice: number | null
    applied_nice: number | null
    requested_io_class: string
    applied_io_class: string | null
    duty_percent: number
    warnings: string[]
    effectiveness_unknown: boolean
  }
}
