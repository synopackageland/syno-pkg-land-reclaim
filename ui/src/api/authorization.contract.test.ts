// @vitest-environment node
// Consumes the authorization contract's immutable handoff (commit b35d10d0):
// schemas/v1/authorization-status.schema.json, ui/src/types/schema/authorization-status.ts
// and tests/fixtures/unsigned-cgi/. These tests are pure schema/fixture
// alignment; they do not exercise DSM itself.
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'
import {
  AUTHORIZATION_REQUIRED_REASONS,
  AUTHORIZATION_STATES,
  AUTHORIZATION_UNAVAILABLE_REASONS,
  createAuthorizationClient,
  type AuthorizationTransport,
  type ContractReason,
  type ContractState,
} from './authorization'
import { createCgiTransport, type CgiFetch } from './cgi'

const read = (relative: string) => JSON.parse(readFileSync(fileURLToPath(new URL(relative, import.meta.url)), 'utf8'))
const schema = read('../../../schemas/v1/authorization-status.schema.json')
const fixture = (name: string) => read(`../../../tests/fixtures/unsigned-cgi/${name}`)

/** The runtime lists and the generated schema type must agree in both directions. */
type Equal<A, B> = [A] extends [B] ? ([B] extends [A] ? true : false) : false

const REQUEST = { method: 'authorization.status', params: {} }
const envelopeTransport = (envelope: unknown): AuthorizationTransport => {
  const fetch: CgiFetch = async () => ({ ok: true, status: 200, text: async () => JSON.stringify(envelope) })
  const transport = createCgiTransport(fetch, { url: 'cgi/api.cgi' })
  return (request, options) => transport(request, options)
}

describe('authorization status contract (b35d10d0)', () => {
  it('keeps the runtime state and reason lists equal to the schema and the generated type', () => {
    const statesMatch: Equal<ContractState, (typeof AUTHORIZATION_STATES)[number]> = true
    const reasonsMatch: Equal<
      ContractReason,
      (typeof AUTHORIZATION_REQUIRED_REASONS)[number] | (typeof AUTHORIZATION_UNAVAILABLE_REASONS)[number] | 'not_admin' | 'session_expired'
    > = true
    expect([statesMatch, reasonsMatch]).toEqual([true, true])

    expect([...AUTHORIZATION_STATES]).toEqual(schema.properties.state.enum)
    expect([...AUTHORIZATION_REQUIRED_REASONS]).toEqual(schema.$defs.authorization_required.properties.reason.enum)
    expect([...AUTHORIZATION_UNAVAILABLE_REASONS]).toEqual(schema.$defs.unavailable.properties.reason.enum)
    expect([...schema.properties.reason.enum].filter(Boolean).sort()).toEqual(
      [...AUTHORIZATION_REQUIRED_REASONS, ...AUTHORIZATION_UNAVAILABLE_REASONS, 'not_admin', 'session_expired'].sort(),
    )
  })

  it('sends exactly the contract request document', async () => {
    const posts: { url: string; body: string }[] = []
    const fetch: CgiFetch = async (input, init) => {
      posts.push({ url: input, body: init.body })
      return { ok: true, status: 200, text: async () => JSON.stringify(fixture('status-ready.json')) }
    }
    await createAuthorizationClient(createCgiTransport(fetch, { url: 'cgi/api.cgi' })).status()
    expect(posts).toEqual([{ url: 'cgi/api.cgi', body: JSON.stringify(REQUEST) }])
    expect(fixture('request-authorization-status.json')).toEqual(REQUEST)
  })

  it.each(['status-ready', 'status-not-admin', 'status-session-expired', 'status-unavailable-device', 'status-authorization-required'])(
    'accepts the %s fixture unchanged',
    async name => {
      const envelope = fixture(`${name}.json`)
      expect(envelope.success).toBe(true)
      await expect(createAuthorizationClient(envelopeTransport(envelope)).status()).resolves.toEqual({
        state: envelope.data.state,
        reason: envelope.data.reason,
      })
    },
  )

  it('maps the paused-executor error fixture to the authorization screen with its reason', async () => {
    await expect(createAuthorizationClient(envelopeTransport(fixture('error-authorization-required.json'))).status()).resolves.toEqual({
      state: 'authorization_required',
      reason: 'upgrade_reapproval',
    })
  })

  it('maps the unavailable auth-service fixture to a retryable reason', async () => {
    await expect(createAuthorizationClient(envelopeTransport(fixture('error-auth-unavailable.json'))).status()).resolves.toEqual({
      state: 'unavailable',
      reason: 'service_unavailable',
    })
  })

  it('keeps a session without a DSM token apart from a retryable outage', async () => {
    const data = { command: 'authorization', schema_version: 1, state: 'unavailable', reason: 'credential_not_ready' }
    await expect(createAuthorizationClient(async () => data).status()).resolves.toEqual({
      state: 'unavailable',
      reason: 'credential_not_ready',
    })
  })

  it('maps the expired-session fixture to the login flow, never to a grant reason', async () => {
    await expect(createAuthorizationClient(envelopeTransport(fixture('error-session-required.json'))).status()).resolves.toEqual({
      state: 'session_expired',
      reason: 'session_expired',
    })
  })

  it('reports an upgrade that outgrew the approval only as a pending authorization', async () => {
    const data = { command: 'authorization', schema_version: 1, state: 'authorization_required', reason: 'upgrade_reapproval' }
    await expect(createAuthorizationClient(async () => data).status()).resolves.toEqual({
      state: 'authorization_required',
      reason: 'upgrade_reapproval',
    })
    await expect(createAuthorizationClient(async () => ({ ...data, state: 'unavailable' })).status()).resolves.toEqual({
      state: 'unavailable',
      reason: null,
    })
  })

  it('drops a reason the state does not allow and maps an unknown state to unavailable', async () => {
    const ready = fixture('status-ready.json').data
    await expect(createAuthorizationClient(async () => ({ ...ready, reason: 'grant_missing' })).status()).resolves.toEqual({
      state: 'ready',
      reason: null,
    })
    await expect(createAuthorizationClient(async () => ({ ...ready, state: 'weird' })).status()).resolves.toEqual({
      state: 'unavailable',
      reason: null,
    })
  })
})
