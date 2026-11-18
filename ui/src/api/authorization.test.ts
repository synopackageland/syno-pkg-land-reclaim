import { describe, expect, it, vi } from 'vitest'
import { ApiError } from './errors'
import { createAuthorizationClient, type AuthorizationTransport } from './authorization'
import { createCgiTransport, type CgiFetch } from './cgi'

/** The `data` of a successful CGI envelope, as tests/fixtures/unsigned-cgi holds it. */
const success = (state: string, reason: string | null) => ({ command: 'authorization', schema_version: 1, state, reason })

describe('authorization client (schemas/v1/authorization-status.schema.json)', () => {
  it('reads the state and reason of authorization.status', async () => {
    const transport: AuthorizationTransport = vi.fn(async () => success('authorization_required', 'grant_missing'))
    await expect(createAuthorizationClient(transport).status()).resolves.toEqual({
      state: 'authorization_required',
      reason: 'grant_missing',
    })
    expect(transport).toHaveBeenCalledWith(
      { method: 'authorization.status', params: {} },
      { signal: undefined },
    )
  })

  it.each([
    ['status-ready', 'ready', null],
    ['status-not-admin', 'not_admin', 'not_admin'],
    ['status-session-expired', 'session_expired', 'session_expired'],
    ['status-unavailable-device', 'unavailable', 'btrfs_device_unavailable'],
  ])('accepts the %s fixture', async (_name, state, reason) => {
    const client = createAuthorizationClient(async () => success(state, reason))
    await expect(client.status()).resolves.toEqual({ state, reason })
  })

  // Diagnostic UID, path and errno belong in the support bundle, never in the
  // screen.
  it('never passes diagnostic detail through', async () => {
    const client = createAuthorizationClient(async () => ({
      ...success('authorization_required', 'helper_untrusted'),
      uid: 0,
      path: '/volume1/@synoreclaim-privilege',
      errno: 13,
    }))
    await expect(client.status()).resolves.toEqual({ state: 'authorization_required', reason: 'helper_untrusted' })
  })

  it('maps an unknown state to unavailable instead of demanding authorization', async () => {
    const client = createAuthorizationClient(async () => success('weird', null))
    await expect(client.status()).resolves.toEqual({ state: 'unavailable', reason: null })
  })

  it('rejects a document that is not this contract, rather than guessing a state', async () => {
    const client = createAuthorizationClient(async () => ({ state: 'unavailable', reason: 'service_unavailable' }))
    await expect(client.status()).resolves.toEqual({ state: 'unavailable', reason: null })
  })

  it('drops a reason the state does not allow', async () => {
    const client = createAuthorizationClient(async () => success('authorization_required', 'not_admin'))
    await expect(client.status()).resolves.toEqual({ state: 'authorization_required', reason: null })
  })

  it('turns the refused-grant error into the authorization screen with its reason', async () => {
    const client = createAuthorizationClient(async () => {
      throw new ApiError('authorization_required', undefined, { cgi_code: 'authorization_required', reason: 'maintenance_task_mismatch' })
    })
    await expect(client.status()).resolves.toEqual({ state: 'authorization_required', reason: 'maintenance_task_mismatch' })
  })

  it('turns a session error into the login flow, not a password prompt', async () => {
    const client = createAuthorizationClient(async () => {
      throw new ApiError('session_timeout')
    })
    await expect(client.status()).resolves.toEqual({ state: 'session_expired', reason: 'session_expired' })
  })

  // The SDK entry gate refuses a non-administrator before the status route.
  it('turns the entry gate refusal into the not-admin screen', async () => {
    const client = createAuthorizationClient(async () => {
      throw new ApiError('no_permission', undefined, { cgi_code: 'admin_required' })
    })
    await expect(client.status()).resolves.toEqual({ state: 'not_admin', reason: 'not_admin' })
  })

  it('turns a temporary auth-service error into a retryable state with its reason', async () => {
    const client = createAuthorizationClient(async () => {
      throw new ApiError('backend_unavailable', undefined, { cgi_code: 'auth_unavailable', reason: 'service_unavailable' })
    })
    await expect(client.status()).resolves.toEqual({ state: 'unavailable', reason: 'service_unavailable' })
  })

  // The gate must not read a raw DSM rejection page as a missing grant.
  it('shows a retryable non-grant state for a DSM HTML 403, never the authorization screen', async () => {
    const fetch: CgiFetch = async () => ({ ok: false, status: 403, text: async () => '<!DOCTYPE html><html><body>Access denied</body></html>' })
    const client = createAuthorizationClient(createCgiTransport(fetch, { url: 'cgi/api.cgi' }))
    await expect(client.status()).resolves.toEqual({ state: 'unavailable', reason: null })
  })

  it('forwards the caller signal', async () => {
    const controller = new AbortController()
    const transport: AuthorizationTransport = vi.fn(async () => success('ready', null))
    await createAuthorizationClient(transport).status({ signal: controller.signal })
    expect(transport).toHaveBeenCalledWith(
      { method: 'authorization.status', params: {} },
      { signal: controller.signal },
    )
  })
})
