/** Every ApiError code. */
export const API_ERROR_CODES = [
  'no_permission',
  'session_timeout',
  'reload',
  'backend_unavailable',
  'auth_unavailable',
  'backend_busy',
  'invalid_param',
  'busy',
  'authorization_required',
  'scan_in_progress',
  'cli_error',
  'timeout',
  'exec_failed',
  'fixture_missing',
  'unknown',
] as const

export type ApiErrorCode = (typeof API_ERROR_CODES)[number]

export class ApiError extends Error {
  constructor(
    readonly code: ApiErrorCode,
    readonly exit?: number,
    readonly detail?: Record<string, unknown>,
  ) {
    super(code)
    this.name = 'ApiError'
  }
}

// The SDK envelope's error codes, and the
// executor's, which the App passes on for a refused root call.
const CGI_CODES: Record<string, ApiErrorCode> = {
  session_required: 'session_timeout',
  admin_required: 'no_permission',
  app_privilege_denied: 'no_permission',
  // DSM could not check the session; the package itself runs.
  auth_unavailable: 'auth_unavailable',
  // The runtime could not start the App, or the App exited.
  app_unavailable: 'backend_unavailable',
  cli_error: 'cli_error',
  bad_request: 'invalid_param',
  request_too_large: 'invalid_param',
  request_timeout: 'timeout',
  // The SDK's origin guard: only a page DSM serves may post.
  cross_origin: 'reload',
  origin_required: 'reload',
  AUTH_DENIED: 'session_timeout',
  ADMIN_REQUIRED: 'no_permission',
  APP_DENIED: 'no_permission',
  INELIGIBLE: 'no_permission',
  ROOT_BUSY: 'backend_busy',
  ROOT_TIMEOUT: 'timeout',
  EXECUTOR_TIMEOUT: 'timeout',
  ROOT_FAILED: 'exec_failed',
  ROOT_STAGING_FULL: 'exec_failed',
  EXECUTOR_TOO_LARGE: 'exec_failed',
  EXECUTOR_PROTOCOL: 'exec_failed',
  // The policy refuses the call, or the executor does not know it: a defect
  // of the package, which approval does not change, so it is an error with
  // its code and asks for nothing.
  POLICY_DENIED: 'exec_failed',
  UNKNOWN_OP: 'exec_failed',
}

/**
 * The refusals the SDK names and checks again on its own: a page
 * without DSM's SynoToken, and a stopped or starting package. The App shows
 * none of them itself; its Gate gets the refusal with the SDK's code.
 */
const SDK_REFUSALS = new Set(['credential_not_ready', 'APP_STOPPED', 'APP_STARTING', 'EXECUTOR_STARTING'])

export function sdkRefusal(error: unknown): { code: string } | undefined {
  const code = error instanceof ApiError ? error.detail?.cgi_code : undefined
  return typeof code === 'string' && SDK_REFUSALS.has(code) ? { code } : undefined
}

/** The executor refusals that need the administrator's approval, and why. */
const APPROVAL_REASONS: Record<string, string> = {
  EXECUTOR_UNAVAILABLE: 'grant_missing',
  EXECUTOR_PAUSED: 'upgrade_reapproval',
}

/**
 * The executor's own code for a refusal the UI cannot name more precisely
 * (ROOT_FAILED, ROOT_STAGING_FULL, ...): shown with the message, so support
 * can tell which operation failed.
 */
export function executorCode(error: ApiError): string | undefined {
  const code = error.detail?.cgi_code
  return (error.code === 'exec_failed' || error.code === 'unknown') && typeof code === 'string' ? code : undefined
}

export function apiErrorFromCgi(code: string, message?: unknown): ApiError {
  const detail: Record<string, unknown> = { cgi_code: code }
  // Reclaim's resume adapter supplies fixed safe reasons and a rescan action.
  // Other backend messages may contain private paths and stay excluded.
  if (['checkpoint_unusable', 'job_incompatible', 'busy', 'not_started', 'finished',
    'no_job', 'not_resumable', 'scan_in_progress', 'resume_failed'].includes(code)
    && typeof message === 'string' && message.length > 0 && message.length <= 512) {
    detail.resume_message = message
  }
  const reason = APPROVAL_REASONS[code]
  if (reason) return new ApiError('authorization_required', undefined, { ...detail, reason })
  const mapped = CGI_CODES[code] ?? 'unknown'
  if (mapped === 'backend_unavailable' || mapped === 'auth_unavailable') detail.reason = 'service_unavailable'
  return new ApiError(mapped, undefined, detail)
}

type CliDocument = { exit?: number; outcome?: string; completion?: string; running_job_id?: string }

/**
 * The CLI refusals a business document carries: a refused argument (exit 2)
 * and a busy filesystem (exit 4). Any other document is the answer.
 */
export function apiErrorFromDocument(value: unknown): ApiError | null {
  if (!value || typeof value !== 'object') return null
  const document = value as CliDocument
  if (document.exit === 2 && document.outcome === 'invalid_param') return new ApiError('invalid_param', 2, { document })
  if (document.exit !== 4 || document.completion !== 'busy') return null
  if (document.outcome === 'scan_in_progress') return new ApiError('scan_in_progress', 4, { running_job_id: document.running_job_id })
  return new ApiError('busy', 4)
}
