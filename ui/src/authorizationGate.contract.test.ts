// Drives the real startup gate with the authorization contract's immutable fixtures
// (b35d10d0): every authorization-status document must land on its screen.
// No native bridge or invented DSM API is involved.
import { flushPromises, mount } from '@vue/test-utils'
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { setBackend } from './api'
import { fixtureBackend } from './api/fixtures'
import { createAuthorizationClient } from './api/authorization'
import { createCgiTransport, type CgiFetch } from './api/cgi'
import { configureAuthorization, resetAuthorization } from './stores/authorization'
import { session } from './stores/session'
import App from './App.vue'
import { elapseGateRetries, fakeGateRetryTimers } from './authorizationFixture'

const fixture = (name: string) => {
  const relative = `../../tests/fixtures/unsigned-cgi/${name}`
  return JSON.parse(readFileSync(fileURLToPath(new URL(relative, import.meta.url)), 'utf8'))
}

/** A client wired to one contract fixture, through the real CGI adapter. */
const clientOf = (envelope: unknown) =>
  createAuthorizationClient((request, options) => {
    const fetch: CgiFetch = async () => ({ ok: true, status: 200, text: async () => JSON.stringify(envelope) })
    return createCgiTransport(fetch, { url: 'cgi/api.cgi' })(request, options)
  })

async function mountGate(name: string, options: { onSessionExpired?: () => void; unavailable?: boolean } = {}) {
  resetAuthorization()
  setBackend(fixtureBackend({ scenario: 'normal' }))
  configureAuthorization({ client: clientOf(fixture(name)), onSessionExpired: options.onSessionExpired ?? (() => {}) })
  if (options.unavailable) fakeGateRetryTimers()
  const wrapper = mount(App, { attachTo: document.body })
  if (options.unavailable) await elapseGateRetries()
  // Follow the session/jobs/volumes lazy imports, as the shell suites do.
  for (let i = 0; i < 50 && !wrapper.find('nav').exists(); i += 1) {
    await flushPromises()
    await vi.dynamicImportSettled()
  }
  await flushPromises()
  return wrapper
}

describe('startup gate over the contract fixtures (b35d10d0)', () => {
  afterEach(() => {
    vi.useRealTimers()
  })

  beforeEach(() => {
    Object.assign(session, {
      status: 'loading', user: null, toolVersion: null,
      cleanupEnabled: false, mutationsDisabled: false, error: null,
    })
  })

  it('enters the app on the ready fixture without asking for a password', async () => {
    const wrapper = await mountGate('status-ready.json')
    expect(wrapper.find('nav').exists()).toBe(true)
    expect(wrapper.findAll('button').some(b=>b.text()==='Authorize')).toBe(false)
    wrapper.unmount()
  })

  it('denies the not-admin fixture without a password', async () => {
    const wrapper = await mountGate('status-not-admin.json')
    expect(wrapper.text()).toContain('Ask a DSM administrators group member to open this App and click “Authorize” here. This is separate from Application Privileges')
    expect(wrapper.findAll('button').some(b=>b.text()==='Authorize')).toBe(false)
    wrapper.unmount()
  })

  it('hands the session-expired fixture to the login flow, never to the password prompt', async () => {
    let expired = 0
    const wrapper = await mountGate('status-session-expired.json', { onSessionExpired: () => { expired += 1 } })
    expect(expired).toBe(1)
    expect(wrapper.text()).toContain('Sign in to DSM again')
    expect(wrapper.findAll('button').some(b=>b.text()==='Authorize')).toBe(false)
    wrapper.unmount()
  })

  it('offers Retry, not Authorize, for the unavailable-device fixture', async () => {
    const wrapper = await mountGate('status-unavailable-device.json', { unavailable: true })
    expect(wrapper.text()).toContain('Authorization could not be checked')
    expect(wrapper.text()).toContain('Try again')
    expect(wrapper.findAll('button').some(b=>b.text()==='Authorize')).toBe(false)
    wrapper.unmount()
  })

  it('shows the authorizing screen with the contract reason for the grant fixtures', async () => {
    const wrapper = await mountGate('status-authorization-required.json')
    expect(wrapper.text()).toContain('Root operations are not authorized yet')
    expect(wrapper.text()).toContain('Authorize')
    wrapper.unmount()

    const error = await mountGate('error-authorization-required.json')
    expect(error.text()).toContain('The package was updated in Package Center, so its previous authorization was revoked')
    expect(error.text()).toContain('Authorize')
    error.unmount()
  })

  it('treats the auth-service error as retryable, and an expired session as login', async () => {
    const unavailable = await mountGate('error-auth-unavailable.json', { unavailable: true })
    expect(unavailable.text()).toContain('Authorization could not be checked')
    unavailable.unmount()

    let expired = 0
    const credentials = await mountGate('error-session-required.json', { onSessionExpired: () => { expired += 1 } })
    expect(expired).toBe(1)
    expect(credentials.text()).toContain('Sign in to DSM again')
    credentials.unmount()
  })
})
