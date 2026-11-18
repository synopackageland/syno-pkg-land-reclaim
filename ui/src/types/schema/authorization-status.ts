// Generated from schemas/v1/authorization-status.schema.json by `pnpm gen:types`. Do not edit.

/**
 * Current authorization status returned by SYNO.SynoReclaim.Authorization.status. The checking state is client-side.
 */
export type SynoreclaimUnsignedCGIAuthorizationStatusV1 = (
  Ready | AuthorizationRequired | NotAdmin | SessionExpired | Unavailable
) & {
  command: 'authorization'
  schema_version: 1
  state: 'ready' | 'authorization_required' | 'not_admin' | 'session_expired' | 'unavailable'
  reason:
    | null
    | 'grant_missing'
    | 'upgrade_reapproval'
    | 'helper_untrusted'
    | 'cli_untrusted'
    | 'grant_incompatible'
    | 'package_identity_changed'
    | 'maintenance_task_missing'
    | 'maintenance_task_mismatch'
    | 'helper_health_rejected'
    | 'capability_missing'
    | 'not_admin'
    | 'session_expired'
    | 'protected_volume_unavailable'
    | 'scheduler_unavailable'
    | 'health_probe_timeout'
    | 'btrfs_device_unavailable'
    | 'service_unavailable'
    | 'credential_not_ready'
}

export interface Ready {
  command: 'authorization'
  schema_version: 1
  state: 'ready'
  reason: null
}
export interface AuthorizationRequired {
  command: 'authorization'
  schema_version: 1
  state: 'authorization_required'
  reason:
    | 'grant_missing'
    | 'upgrade_reapproval'
    | 'helper_untrusted'
    | 'cli_untrusted'
    | 'grant_incompatible'
    | 'package_identity_changed'
    | 'maintenance_task_missing'
    | 'maintenance_task_mismatch'
    | 'helper_health_rejected'
    | 'capability_missing'
}
export interface NotAdmin {
  command: 'authorization'
  schema_version: 1
  state: 'not_admin'
  reason: 'not_admin'
}
export interface SessionExpired {
  command: 'authorization'
  schema_version: 1
  state: 'session_expired'
  reason: 'session_expired'
}
export interface Unavailable {
  command: 'authorization'
  schema_version: 1
  state: 'unavailable'
  reason:
    | 'protected_volume_unavailable'
    | 'scheduler_unavailable'
    | 'health_probe_timeout'
    | 'btrfs_device_unavailable'
    | 'service_unavailable'
    | 'credential_not_ready'
}
