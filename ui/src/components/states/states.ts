import type { SessionStatus } from '@/stores/session'

/** The global states the UI can show. */
export type StateKind =
  | 'not_admin'
  | 'backend_unavailable'
  | 'session_expired'
  | 'no_supported_volume'
  | 'no_report'
  | 'partial'
  | 'busy'
  | 'error'

/** The state that replaces every page, or null (checked in SPEC §3 order). */
export function blockingState(session: SessionStatus, volumesDoc: { volumes: { filesystem?: string; capabilities?: { scan: boolean } }[] } | null): StateKind | null {
  if (session === 'not_admin' || session === 'backend_unavailable') return session
  if (volumesDoc && !volumesDoc.volumes.some(volume => volume.capabilities?.scan !== false)) return 'no_supported_volume'
  return null
}
