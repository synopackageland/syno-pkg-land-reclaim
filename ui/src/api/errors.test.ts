import { ApiError, apiErrorFromCgi, apiErrorFromDocument, executorCode, sdkRefusal } from './errors'

describe('executorCode', () => {
  it('names the executor code a catch-all refusal stands for', () => {
    expect(executorCode(apiErrorFromCgi('ROOT_FAILED'))).toBe('ROOT_FAILED')
    expect(executorCode(apiErrorFromCgi('ROOT_STAGING_FULL'))).toBe('ROOT_STAGING_FULL')
    expect(executorCode(apiErrorFromCgi('POLICY_DENIED'))).toBe('POLICY_DENIED')
    expect(executorCode(apiErrorFromCgi('internal'))).toBe('internal')
  })

  it('names nothing for a refusal its message already explains', () => {
    expect(executorCode(apiErrorFromCgi('AUTH_DENIED'))).toBeUndefined()
    expect(executorCode(apiErrorFromCgi('ROOT_BUSY'))).toBeUndefined()
    expect(executorCode(new ApiError('exec_failed'))).toBeUndefined()
  })
})

describe('apiErrorFromCgi', () => {
  it.each([
    ['session_required', 'session_timeout'],
    ['admin_required', 'no_permission'],
    ['app_privilege_denied', 'no_permission'],
    // DSM could not check the session: the package runs, so it is not stopped.
    ['auth_unavailable', 'auth_unavailable'],
    // The runtime could not start the App, or it exited: the package is broken.
    ['app_unavailable', 'backend_unavailable'],
    ['cli_error', 'cli_error'],
    ['AUTH_DENIED', 'session_timeout'],
    ['ADMIN_REQUIRED', 'no_permission'],
    ['APP_DENIED', 'no_permission'],
    ['INELIGIBLE', 'no_permission'],
    ['ROOT_BUSY', 'backend_busy'],
    ['ROOT_TIMEOUT', 'timeout'],
    ['EXECUTOR_TIMEOUT', 'timeout'],
    ['ROOT_FAILED', 'exec_failed'],
    ['ROOT_STAGING_FULL', 'exec_failed'],
    ['EXECUTOR_TOO_LARGE', 'exec_failed'],
    ['EXECUTOR_PROTOCOL', 'exec_failed'],
    // A policy that refuses the call, or an executor that does not know it, is
    // the package's defect: approval does not change it.
    ['POLICY_DENIED', 'exec_failed'],
    ['UNKNOWN_OP', 'exec_failed'],
    ['bad_request', 'invalid_param'],
    ['request_too_large', 'invalid_param'],
    ['request_timeout', 'timeout'],
    ['cross_origin', 'reload'],
    ['origin_required', 'reload'],
    ['bad_response', 'unknown'],
  ])('maps the SDK code %s to %s', (cgi, code) => {
    expect(apiErrorFromCgi(cgi).code).toBe(code)
  })

  // The executor not answering needs the administrator's approval again; a
  // paused one waits for it after an upgrade.
  it.each([
    ['EXECUTOR_UNAVAILABLE', 'grant_missing'],
    ['EXECUTOR_PAUSED', 'upgrade_reapproval'],
  ])('asks for approval on %s with the reason %s', (code, reason) => {
    const error = apiErrorFromCgi(code)
    expect(error.code).toBe('authorization_required')
    expect(error.detail).toEqual({ cgi_code: code, reason })
  })

  it('reports auth_unavailable as the retryable service_unavailable reason', () => {
    expect(apiErrorFromCgi('auth_unavailable').detail).toEqual({ cgi_code: 'auth_unavailable', reason: 'service_unavailable' })
  })

  it('keeps an unrecognised code as detail', () => {
    const error = apiErrorFromCgi('internal')
    expect(error).toBeInstanceOf(ApiError)
    expect(error.code).toBe('unknown')
    expect(error.detail).toEqual({ cgi_code: 'internal' })
  })
})

// The SDK names these itself and checks again on its own: a page without
// DSM's SynoToken, and a stopped or starting package.
describe('sdkRefusal', () => {
  it.each(['credential_not_ready', 'APP_STOPPED', 'APP_STARTING', 'EXECUTOR_STARTING'])('leaves %s to the SDK with its own code', code => {
    expect(sdkRefusal(apiErrorFromCgi(code))).toEqual({ code })
  })

  it.each(['backend_unavailable', 'auth_unavailable', 'ROOT_FAILED', 'EXECUTOR_UNAVAILABLE'])('keeps %s for the App', code => {
    expect(sdkRefusal(apiErrorFromCgi(code))).toBeUndefined()
  })

  it('keeps an error that is not an ApiError for the App', () => {
    expect(sdkRefusal(new Error('APP_STOPPED'))).toBeUndefined()
  })
})

describe('apiErrorFromDocument', () => {
  it('refuses a CLI invalid_param document with its exit code', () => {
    const document = { outcome: 'invalid_param', exit: 2 }
    expect(apiErrorFromDocument(document)).toMatchObject({ code: 'invalid_param', exit: 2, detail: { document } })
  })

  it('keeps the scan admission refusal and running job id', () => {
    const error = apiErrorFromDocument({ completion: 'busy', outcome: 'scan_in_progress', running_job_id: 's1', exit: 4 })
    expect(error?.code).toBe('scan_in_progress')
    expect(error?.detail?.running_job_id).toBe('s1')
  })

  it('reports another busy run as busy', () => {
    expect(apiErrorFromDocument({ completion: 'busy', exit: 4 })).toMatchObject({ code: 'busy', exit: 4 })
  })

  it('passes every other document through', () => {
    expect(apiErrorFromDocument({ exit: 7 })).toBeNull()
    expect(apiErrorFromDocument({ outcome: 'invalid_param', exit: 0 })).toBeNull()
    expect(apiErrorFromDocument(null)).toBeNull()
  })
})


it('retains safe resume explanations only for the app resume refusal codes', () => {
  expect(apiErrorFromCgi('metadata_error', '/private/file').detail).not.toHaveProperty('resume_message')
  expect(apiErrorFromCgi('checkpoint_unusable', 'x'.repeat(513)).detail).not.toHaveProperty('resume_message')
  expect(apiErrorFromCgi('checkpoint_unusable', { path: '/private/file' }).detail).not.toHaveProperty('resume_message')
})
