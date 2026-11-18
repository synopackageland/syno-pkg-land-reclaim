// The authorization check every app opening runs before jobs, volumes or any
// root operation.
// The server's state/reason contract is schemas/v1/authorization-status.schema.json;
// only a state and a user-facing reason
// cross this boundary, never diagnostic UID/path/errno detail.
import { ApiError } from './errors'
import type { SynoreclaimUnsignedCGIAuthorizationStatusV1 as AuthorizationStatusDocument } from '@/types/schema/authorization-status'

/** The authorization contract's generated schema type is the source of truth; the lists below must match it. */
export type ContractStatus = AuthorizationStatusDocument
export type ContractState = AuthorizationStatusDocument['state']
export type ContractReason = NonNullable<AuthorizationStatusDocument['reason']>

/** The contract's fixed API for the status call. */
export const AUTHORIZATION_API = 'SYNO.SynoReclaim.Authorization'

export const AUTHORIZATION_STATES = ['ready', 'authorization_required', 'not_admin', 'session_expired', 'unavailable'] as const
export type AuthorizationState = (typeof AUTHORIZATION_STATES)[number]

/** `authorization_required` reasons: a known grant defect needs repair. */
export const AUTHORIZATION_REQUIRED_REASONS = [
  'grant_missing',
  'upgrade_reapproval',
  'helper_untrusted',
  'cli_untrusted',
  'grant_incompatible',
  'package_identity_changed',
  'maintenance_task_missing',
  'maintenance_task_mismatch',
  'helper_health_rejected',
  'capability_missing',
] as const
export type AuthorizationRequiredReason = (typeof AUTHORIZATION_REQUIRED_REASONS)[number]

/** `unavailable` reasons: the check cannot decide; retry, never a password. */
export const AUTHORIZATION_UNAVAILABLE_REASONS = [
  'protected_volume_unavailable',
  'scheduler_unavailable',
  'health_probe_timeout',
  'btrfs_device_unavailable',
  'service_unavailable',
  'credential_not_ready',
] as const
export type AuthorizationUnavailableReason = (typeof AUTHORIZATION_UNAVAILABLE_REASONS)[number]

export type AuthorizationReason = AuthorizationRequiredReason | AuthorizationUnavailableReason | 'not_admin' | 'session_expired'

const REASONS_BY_STATE: Record<AuthorizationState, readonly string[] | null> = {
  ready: null,
  authorization_required: AUTHORIZATION_REQUIRED_REASONS,
  not_admin: ['not_admin'],
  session_expired: ['session_expired'],
  unavailable: AUTHORIZATION_UNAVAILABLE_REASONS,
}

export type AuthorizationStatus = { state: AuthorizationState; reason: AuthorizationReason | null }

export type AuthorizationRequest = { method: 'authorization.status'; params: Record<string, never> }
export type AuthorizationTransport = (request: AuthorizationRequest, options: { signal?: AbortSignal }) => Promise<unknown>

export interface AuthorizationClient {
  status(options?: { signal?: AbortSignal }): Promise<AuthorizationStatus>
}

/**
 * Until the package CGI client is wired (lane A's URL and credential carrier),
 * the gate reports unavailable so the window offers Retry. It never reports a
 * fake ready.
 */
export const unavailableAuthorizationClient: AuthorizationClient = {
  status: async () => ({ state: 'unavailable', reason: null }),
}

/** The checking state is client-side; the server returns only these five. */
export function isAuthorizationState(value: unknown): value is AuthorizationState {
  return typeof value === 'string' && (AUTHORIZATION_STATES as readonly string[]).includes(value)
}

function reasonOf(state: AuthorizationState, value: unknown): AuthorizationReason | null {
  const allowed = REASONS_BY_STATE[state]
  return allowed && typeof value === 'string' && allowed.includes(value) ? (value as AuthorizationReason) : null
}

function statusFromDocument(document: unknown): AuthorizationStatus {
  if (!document || typeof document !== 'object') return { state: 'unavailable', reason: null }
  const record = document as Record<string, unknown>
  // A wrong or unknown document is a transport problem: retry, never a
  // password prompt.
  if (record.command !== 'authorization' || record.schema_version !== 1) return { state: 'unavailable', reason: null }
  if (!isAuthorizationState(record.state)) return { state: 'unavailable', reason: null }
  return { state: record.state, reason: reasonOf(record.state, record.reason) }
}

/**
 * A refused grant, an entry gate refusal, an expired session or an
 * unavailable service is still a state.
 */
function statusFromError(error: unknown): AuthorizationStatus | null {
  if (!(error instanceof ApiError)) return null
  const reason = typeof error.detail?.reason === 'string' ? error.detail.reason : null
  if (error.code === 'authorization_required') return { state: 'authorization_required', reason: reasonOf('authorization_required', reason) }
  if (error.code === 'no_permission') return { state: 'not_admin', reason: 'not_admin' }
  if (error.code === 'session_timeout') return { state: 'session_expired', reason: 'session_expired' }
  if (error.code === 'backend_unavailable' || error.code === 'auth_unavailable') return { state: 'unavailable', reason: reasonOf('unavailable', reason) }
  return null
}

export function createAuthorizationClient(transport: AuthorizationTransport): AuthorizationClient {
  return {
    async status(options) {
      try {
        const document = await transport({ method: 'authorization.status', params: {} }, { signal: options?.signal })
        return statusFromDocument(document)
      } catch (error) {
        const status = statusFromError(error)
        if (status) return status
        throw error
      }
    },
  }
}
