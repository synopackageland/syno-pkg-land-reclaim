import { flushPromises, mount } from '@vue/test-utils'
import { setBackend, api, ApiError } from './api'
import { apiErrorFromCgi } from './api/errors'
import type { Backend, Method } from './api/methods'
import { cgiBackend, createCallTransport, type WindowCall } from './api/cgi'
import { createAuthorizationClient } from './api/authorization'
import { fixtureBackend } from './api/fixtures'
import { fixtureAuthorizationClient, sequenceAuthorizationClient } from './api/authorizationFixtures'
import { configureAuthorization, resetAuthorization, type AuthorizationAccess } from './stores/authorization'
import App from './App.vue'
import { Button, LoadingArea } from '@synopackageland/ui'
import { ExecutorApproval, ExecutorSetupKey, type ExecutorSetup } from '@synopackageland/sdk/vue'
import { nav, navigate } from './stores/nav'
import { session } from './stores/session'
import { jobs } from './stores/jobs'
import { type AuthorizationClient, type AuthorizationStatus } from './api/authorization'
import { elapseGateRetries, fakeGateRetryTimers } from './authorizationFixture'

const READY: AuthorizationStatus = { state: 'ready', reason: null }

type MountOptions = {
  scenario?: string
  auth?: AuthorizationStatus
  client?: AuthorizationClient
  access?: AuthorizationAccess
  backend?: Backend
  onSessionExpired?: () => void
  executor?: ExecutorSetup
}

/** Mounts the app, recording every business method the pages ask for. */
async function mountApp(options: MountOptions = {}) {
  const inner: Backend = options.backend ?? fixtureBackend({ scenario: options.scenario ?? 'normal' })
  const calls: string[] = []
  const backend: Backend = (method, params, opts) => {
    calls.push(method)
    return inner(method, params, opts)
  }
  setBackend(backend)
  configureAuthorization({
    client: options.client ?? fixtureAuthorizationClient(options.auth ?? READY),
    access: options.access,
    onSessionExpired: options.onSessionExpired ?? (() => {}),
  })
  const provide = options.executor ? { [ExecutorSetupKey as symbol]: options.executor } : {}
  const wrapper = mount(App, { attachTo: document.body, global: { provide } })
  await settle()
  return { wrapper, calls }
}

/** A client that answers each status in turn, throwing the ApiErrors; the last one stays. */
function sequenceAuthorizationClientOf(...answers: Array<AuthorizationStatus | ApiError>): AuthorizationClient {
  let next = 0
  return {
    async status() {
      const answer = answers[Math.min(next++, answers.length - 1)]
      if (answer instanceof ApiError) throw answer
      return answer
    },
  }
}

/** Waits for the Vue scheduler, the fixture backend's lazy JSON modules and their promises. */
async function settle() {
  await flushPromises()
  await vi.dynamicImportSettled()
  await flushPromises()
}

function buttonWith(wrapper: ReturnType<typeof mount>, label: string) {
  return wrapper.findAllComponents(Button).find(button => button.text().includes(label))
}

describe('App startup authorization gate', () => {
  beforeEach(() => {
    sessionStorage.clear()
    nav.page = 'overview'
    nav.newScanOpen = false
    resetAuthorization()
    // The stores are module state: an earlier test may have left them ready.
    Object.assign(session, {
      status: 'loading', user: null, toolVersion: null,
      cleanupEnabled: false, mutationsDisabled: false, error: null,
    })
  })

  afterEach(() => {
    vi.useRealTimers()
  })

  it('centres the loading state while the session metadata loads after a ready check', async () => {
    const { wrapper } = await mountApp({ backend: () => new Promise(() => {}) })
    expect(wrapper.findComponent(LoadingArea).exists()).toBe(true)
    expect(wrapper.text()).toContain('Loading')
    wrapper.unmount()
  })

  it('renders the sidebar, toolbar and the current page once authorization is ready', async () => {
    const { wrapper } = await mountApp()
    for (const label of ['Overview', 'Explore', 'Snapshot', 'Recycle Bin', 'History', 'New scan']) {
      expect(wrapper.text()).toContain(label)
    }
    expect(wrapper.find('[data-ui-app]').attributes('lang')).toBe('en')
    wrapper.unmount()
  })

  it('tells the executor approval why the app needs system privileges', async () => {
    const executor: ExecutorSetup = { status: vi.fn(async () => ({ state: 'needs-admin-approval' as const })), authorize: vi.fn(), control: vi.fn() }
    const { wrapper } = await mountApp({ executor })
    const approval = wrapper.findComponent(ExecutorApproval)
    expect(approval.props('purpose')).toBe(
      "It needs system privileges to measure the space used by every shared folder, snapshot and package, and to move only what you confirm into its shared folder's Recycle Bin, or delete it for good when that Recycle Bin is off or it is in no shared folder",
    )
    expect(wrapper.get('[data-executor-consent]').text()).toContain(approval.props('purpose'))
    wrapper.unmount()
  })

  it('switches the page from the sidebar and remembers it', async () => {
    const { wrapper } = await mountApp()
    const explore = wrapper.findAll('nav button').find(b => b.text() === 'Explore')!
    await explore.trigger('click')
    expect(nav.page).toBe('explore')
    expect(sessionStorage.getItem('synoreclaim.page')).toBe('"explore"')
    wrapper.unmount()
  })

  it('shows only the not-admin state to non-administrators', async () => {
    const { wrapper } = await mountApp({ auth: { state: 'not_admin', reason: null } })
    expect(wrapper.text()).toContain('Ask a DSM administrators group member to open this App and click “Authorize” here. This is separate from Application Privileges')
    expect(wrapper.find('nav').exists()).toBe(false)
    expect(buttonWith(wrapper, 'Authorize')).toBeUndefined()
    wrapper.unmount()
  })

  it('shows the job strip while a scan runs', async () => {
    const { wrapper } = await mountApp({ scenario: 'scanning' })
    expect(wrapper.text()).toContain('Whole volume1 · Scanning')
    wrapper.unmount()
  })

  it('opens the new scan dialog when no volume has a report', async () => {
    const { wrapper } = await mountApp({ scenario: 'no_report' })
    expect(nav.newScanOpen).toBe(true)
    wrapper.unmount()
  })

  it('says there is no Btrfs volume, lists the skipped volumes and offers no scan', async () => {
    const { wrapper } = await mountApp({ scenario: 'no_supported_volume' })
    expect(wrapper.text()).toContain('No supported volume')
    expect(wrapper.text()).toContain('Syno Reclaim needs a Btrfs or ext4 volume')
    expect(wrapper.findAll('[role="list"] [data-ui-list-item-title]').map(title => title.text())).toEqual(['/volume1', '/volume2'])
    expect(wrapper.find('[role="list"] [data-ui-list-item-description]').text()).toBe('Unsupported file system')
    expect(wrapper.find('nav').exists()).toBe(false)
    expect(buttonWith(wrapper, 'Scan')).toBeUndefined()
    expect(nav.newScanOpen).toBe(false)
    wrapper.unmount()
  })

  it('waits for ready before jobs, volumes or the first scan start', async () => {
    const { wrapper, calls } = await mountApp({ auth: { state: 'authorization_required', reason: 'grant_missing' } })
    expect(calls).toEqual([])
    expect(nav.newScanOpen).toBe(false)
    expect(wrapper.text()).toContain('Syno Reclaim needs your permission')
    expect(wrapper.find('nav').exists()).toBe(false)
    wrapper.unmount()
  })

  it('enters only after the common host request returns fresh ready', async () => {
    const access: AuthorizationAccess = {
      status: async () => ({command:'authorization',schema_version:1,state:'authorization_required',reason:'grant_missing'}),
      request: async () => ({command:'authorization',schema_version:1,state:'ready',reason:null}),
      revoke: async () => ({command:'authorization',schema_version:1,state:'authorization_required',reason:'grant_missing'}),
    }
    const {wrapper,calls}=await mountApp({access})
    expect(calls).toEqual([])
    await buttonWith(wrapper,'Authorize')!.trigger('click');await settle()
    expect(wrapper.find('nav').exists()).toBe(true)
    expect(calls).toContain('volumes.list');wrapper.unmount()
  })

  // The first open took 9.6 s; jobs and volumes are independent reads.
  it('asks for jobs and volumes together once the session is ready', async () => {
    const inner = fixtureBackend({ scenario: 'normal' })
    const { wrapper, calls } = await mountApp({
      backend: (method, params, opts) => method === 'jobs.list' ? new Promise(() => {}) : inner(method, params, opts),
    })
    expect(calls).toContain('jobs.list')
    expect(calls).toContain('volumes.list')
    wrapper.unmount()
  })

  // PERF-STANDALONE: the session, jobs and volumes were three round trips in a row, the volumes twice.
  it('asks for the session, jobs and volumes together, not one after another', async () => {
    const inner = fixtureBackend({ scenario: 'normal' })
    const { wrapper, calls } = await mountApp({
      backend: (method, params, opts) => method === 'session.get' ? new Promise(() => {}) : inner(method, params, opts),
    })
    expect(calls).toContain('jobs.list')
    expect(calls).toContain('volumes.list')
    wrapper.unmount()
  })

  it('reads the session, jobs and volumes once each on open', async () => {
    // The fixtures' volumes carry a fixed old time; a NAS answers with the time of the read.
    const inner = fixtureBackend({ scenario: 'normal' })
    const { wrapper, calls } = await mountApp({
      backend: async (method, params, opts) => {
        const answer = await inner(method, params, opts)
        return method === 'volumes.list' ? { ...answer, generated_at: new Date().toISOString() } as never : answer
      },
    })
    for (const method of ['session.get', 'jobs.list', 'volumes.list']) {
      expect(calls.filter(call => call === method), method).toHaveLength(1)
    }
    wrapper.unmount()
  })

  it('reads the volumes again when a scan ended after they were read', async () => {
    const inner = fixtureBackend({ scenario: 'normal' })
    const { wrapper, calls } = await mountApp({
      backend: async (method, params, opts) => {
        const answer = await inner(method, params, opts)
        return method === 'volumes.list' ? { ...answer, generated_at: '2000-01-01T00:00:00Z' } as never : answer
      },
    })
    expect(calls.filter(call => call === 'volumes.list').length).toBeGreaterThan(1)
    wrapper.unmount()
  })

  it('shows the sidebar and the page title while the first reads are still on their way', async () => {
    const { wrapper } = await mountApp({ backend: () => new Promise(() => {}) })
    expect(wrapper.find('nav').exists()).toBe(true)
    expect(wrapper.findComponent(LoadingArea).exists()).toBe(true)
    expect(wrapper.text()).toContain('Overview')
    expect(buttonWith(wrapper, 'New scan')).toBeUndefined()
    wrapper.unmount()
  })

  it('retries a temporary check without asking for a password', async () => {
    fakeGateRetryTimers()
    const { wrapper, calls } = await mountApp({ client: sequenceAuthorizationClient({ state: 'unavailable', reason: null }, { state: 'unavailable', reason: null }, { state: 'unavailable', reason: null }, READY) })
    await elapseGateRetries()
    await settle()
    expect(buttonWith(wrapper, 'Authorize')).toBeUndefined()
    expect(wrapper.text()).toContain('Authorization could not be checked')

    await buttonWith(wrapper, 'Try again')!.trigger('click')
    await settle()
    expect(wrapper.find('nav').exists()).toBe(true)
    expect(calls).toContain('volumes.list')
    wrapper.unmount()
  })

  // A portal page without DSM's SynoToken said Loading for over 15 s
  // first; the refusal cannot pass by itself, so the Gate shows it at once.
  it('asks to sign in to DSM again at once, without checking again, when the page has no SynoToken', async () => {
    fakeGateRetryTimers()
    const status = vi.fn(async (): Promise<AuthorizationStatus> => { throw apiErrorFromCgi('credential_not_ready') })
    const { wrapper, calls } = await mountApp({ client: { status } })
    expect(wrapper.text()).toContain("DSM's sign-in credential is not ready")
    expect(status).toHaveBeenCalledOnce()
    expect(calls).toEqual([])
    wrapper.unmount()
  })

  // A stopped package is the SDK's to say and to check again.
  it('shows the SDK lifecycle for a stopped package and enters once it runs', async () => {
    fakeGateRetryTimers()
    const stopped = apiErrorFromCgi('APP_STOPPED')
    const { wrapper } = await mountApp({ client: sequenceAuthorizationClientOf(stopped, stopped, stopped, READY) })
    await elapseGateRetries()
    await settle()
    expect(wrapper.text()).toContain('Syno Reclaim is stopped')
    expect(wrapper.text()).not.toContain('stopped or damaged')
    await vi.advanceTimersByTimeAsync(2000)
    await settle()
    expect(wrapper.find('nav').exists()).toBe(true)
    wrapper.unmount()
  })

  it('hands a call refused by a stopped package to the SDK lifecycle and loads again once it runs', async () => {
    fakeGateRetryTimers()
    const inner = fixtureBackend({ scenario: 'normal' })
    let stopped = true
    const calls: string[] = []
    const backend: Backend = (method, params, opts) => {
      calls.push(method)
      return method === 'session.get' && stopped ? Promise.reject(apiErrorFromCgi('APP_STOPPED')) : inner(method, params, opts)
    }
    const status = vi.fn(async (): Promise<AuthorizationStatus> => {
      if (calls.length && stopped) throw apiErrorFromCgi('APP_STOPPED')
      return READY
    })
    const { wrapper } = await mountApp({ backend, client: { status } })
    await elapseGateRetries()
    await settle()
    expect(wrapper.text()).toContain('Syno Reclaim is stopped')
    expect(wrapper.text()).not.toContain('stopped or damaged')
    stopped = false
    await vi.advanceTimersByTimeAsync(2000)
    await settle()
    expect(wrapper.find('nav').exists()).toBe(true)
    expect(calls.filter(method => method === 'session.get')).toHaveLength(2)
    wrapper.unmount()
  })

  // A page's own call refused while the package stops read
  // "unknown error (APP_STOPPED)" with a retry under the Gate's dialog.
  // As the file's first visit to Explore it pays that page's first import:
  // ~0.6 s alone, past 5 s on a gate host that also runs the Rust builds.
  it('leaves a page call refused by a stopped package to the SDK lifecycle and loads the page again once it runs', async () => {
    fakeGateRetryTimers()
    const inner = fixtureBackend({ scenario: 'normal' })
    let stopped = false
    const calls: string[] = []
    const backend: Backend = (method, params, opts) => {
      calls.push(method)
      return method === 'top.list' && stopped ? Promise.reject(apiErrorFromCgi('APP_STOPPED')) : inner(method, params, opts)
    }
    const status = vi.fn(async (): Promise<AuthorizationStatus> => {
      if (stopped) throw apiErrorFromCgi('APP_STOPPED')
      return READY
    })
    const { wrapper } = await mountApp({ backend, client: { status } })
    stopped = true
    navigate('explore')
    await settle()
    await elapseGateRetries()
    await settle()
    expect(wrapper.text()).toContain('Syno Reclaim is stopped')
    expect(wrapper.text()).not.toContain('APP_STOPPED')
    expect(buttonWith(wrapper, 'Retry')).toBeUndefined()
    const asked = calls.filter(method => method === 'top.list').length
    stopped = false
    await vi.advanceTimersByTimeAsync(2000)
    // Recovery also starts the UI transition before its lazy page settles.
    await flushPromises()
    await vi.runOnlyPendingTimersAsync()
    await settle()
    expect(wrapper.find('nav').exists()).toBe(true)
    expect(calls.filter(method => method === 'top.list').length).toBeGreaterThan(asked)
    expect(wrapper.text()).not.toContain('APP_STOPPED')
    wrapper.unmount()
  }, 30_000)

  it('offers to authorize again, without loading root data, after the dialog is dismissed', async () => {
    const { wrapper, calls } = await mountApp({ auth: { state: 'authorization_required', reason: 'grant_missing' } })
    await wrapper.find('[data-ui-dialog-close], button[aria-label="Close"]').trigger('click')
    await settle()
    expect(wrapper.text()).not.toContain('Syno Reclaim needs your permission')
    expect(wrapper.text()).toContain('Not authorized')
    expect(calls).toEqual([])

    await buttonWith(wrapper, 'Authorize')!.trigger('click')
    await settle()
    expect(wrapper.text()).toContain('Syno Reclaim needs your permission')
    wrapper.unmount()
  })

  it('returns to the DSM login on an expired session instead of demanding authorization', async () => {
    let expired = 0
    const { wrapper } = await mountApp({ client: fixtureAuthorizationClient({ state: 'session_expired', reason: null }), onSessionExpired: () => { expired += 1 } })
    expect(expired).toBe(1)
    expect(wrapper.text()).toContain('Sign in to DSM again')
    expect(buttonWith(wrapper, 'Authorize')).toBeUndefined()
    wrapper.unmount()
  })

  it('shows a policy refusal as an error and does not ask for approval or read it back', async () => {
    const calls: string[] = []
    let statuses = 0
    const backend: Backend = method => {
      calls.push(method)
      return Promise.reject(apiErrorFromCgi('POLICY_DENIED'))
    }
    const access = {
      status: async () => { statuses += 1; return { command: 'authorization' as const, schema_version: 1 as const, state: 'ready' as const, reason: null } },
      request: async () => ({ command: 'authorization' as const, schema_version: 1 as const, state: 'ready' as const, reason: null }),
      revoke: async () => ({ command: 'authorization' as const, schema_version: 1 as const, state: 'authorization_required' as const, reason: 'grant_missing' as const }),
    }
    const { wrapper } = await mountApp({ backend, access })
    const read = statuses
    expect(wrapper.find('nav').exists()).toBe(false)
    expect(wrapper.text()).toContain('Something went wrong')
    expect(wrapper.text()).toContain('A system operation failed (error code: POLICY_DENIED)')
    expect(wrapper.text()).not.toContain('needs your permission')

    await buttonWith(wrapper, 'Retry')!.trigger('click')
    await settle()
    expect(calls.filter(method => method === 'session.get')).toHaveLength(2)
    expect(statuses).toBe(read)
    wrapper.unmount()
  })

  it('shows the refusal, not empty pages, when the volumes cannot be listed', async () => {
    const inner = fixtureBackend({ scenario: 'normal' })
    const backend: Backend = (method, params, opts) =>
      method === 'volumes.list' ? Promise.reject(apiErrorFromCgi('ROOT_FAILED')) : inner(method, params, opts)
    const { wrapper } = await mountApp({ backend })
    expect(wrapper.find('nav').exists()).toBe(false)
    expect(wrapper.text()).toContain('Something went wrong')
    expect(wrapper.text()).toContain('A system operation failed (error code: ROOT_FAILED)')
    expect(wrapper.text()).not.toContain('stopped or damaged')
    expect(buttonWith(wrapper, 'Retry')).toBeDefined()
    wrapper.unmount()
  })

  it('says a system operation failed, not that the package stopped, when session.get fails as root', async () => {
    const inner = fixtureBackend({ scenario: 'normal' })
    const backend: Backend = (method, params, opts) =>
      method === 'session.get' ? Promise.reject(apiErrorFromCgi('ROOT_FAILED')) : inner(method, params, opts)
    const { wrapper } = await mountApp({ backend })
    expect(wrapper.text()).toContain('A system operation failed (error code: ROOT_FAILED)')
    expect(wrapper.text()).not.toContain('Package Center')
    expect(buttonWith(wrapper, 'Retry')).toBeDefined()
    wrapper.unmount()
  })

  it('stops root work when a call reports missing authorization and does not replay it after reauthorizing', async () => {
    const inner = fixtureBackend({ scenario: 'normal' })
    const calls: string[] = []
    const backend: Backend = (method, params, opts) => {
      calls.push(method)
      if (method === 'cleanup.apply') return Promise.reject(new ApiError('authorization_required'))
      return inner(method, params, opts)
    }
    const { wrapper } = await mountApp({ backend, access: {status:async()=>({command:'authorization',schema_version:1,state:calls.includes('cleanup.apply')?'authorization_required':'ready',reason:'grant_missing'}),request:async()=>({command:'authorization',schema_version:1,state:'ready',reason:null}),revoke:async()=>({command:'authorization',schema_version:1,state:'authorization_required',reason:'grant_missing'})} })

    await expect(api('cleanup.apply', { plan_id: 'plan-1', confirm_move: true })).rejects.toMatchObject({ code: 'authorization_required' })
    await settle()
    expect(wrapper.find('nav').exists()).toBe(true)
    expect(wrapper.text()).toContain('Syno Reclaim needs your permission')

    await buttonWith(wrapper, 'Authorize')!.trigger('click')
    await settle()
    expect(wrapper.find('nav').exists()).toBe(true)
    expect(calls.filter(method => method === 'cleanup.apply')).toHaveLength(1)
    wrapper.unmount()
  })
})

/**
 * The DSM window's ctx.call as the SDK host answers a stopped package:
 * at once, rejecting with the CGI envelope's own error object, not an ApiError.
 * The App reaches it through the same transports its DSM bundle builds.
 */
function stoppableWindow({ scenario = 'normal', stopAfter, refuseOnce }: { scenario?: string; stopAfter?: Method; refuseOnce?: Method } = {}) {
  const inner = fixtureBackend({ scenario })
  const window = { stopped: false, calls: [] as string[] }
  const call: WindowCall = async (method, params, opts) => {
    window.calls.push(method)
    if (window.stopped || method === refuseOnce) {
      if (method === refuseOnce) refuseOnce = undefined
      throw { code: 'APP_STOPPED', message: 'The package is stopped' }
    }
    if (method === 'authorization.status') return { command: 'authorization', schema_version: 1, ...READY }
    try {
      return await inner(method as Method, params as never, opts)
    } finally {
      if (method === stopAfter) {
        window.stopped = true
        stopAfter = undefined
      }
    }
  }
  const transport = createCallTransport(call)
  return { window, backend: cgiBackend(transport), client: createAuthorizationClient(transport) }
}

describe("App over the DSM window's ctx.call", () => {
  beforeEach(() => {
    sessionStorage.clear()
    nav.page = 'overview'
    nav.newScanOpen = false
    resetAuthorization()
    Object.assign(session, {
      status: 'loading', user: null, toolVersion: null,
      cleanupEnabled: false, mutationsDisabled: false, error: null,
    })
    jobs.list = []
    jobs.error = null
  })

  afterEach(() => {
    vi.useRealTimers()
  })

  it('shows the stopped package when the job poll on the Overview is refused, and enters again once it runs', async () => {
    fakeGateRetryTimers()
    const { window, backend, client } = stoppableWindow()
    const { wrapper } = await mountApp({ backend, client })
    expect(wrapper.find('nav').exists()).toBe(true)
    window.stopped = true
    await vi.advanceTimersByTimeAsync(60_000)
    await settle()
    await elapseGateRetries()
    await settle()
    expect(wrapper.text()).toContain('Syno Reclaim is stopped')
    expect(window.calls).toContain('authorization.status')
    window.stopped = false
    await vi.advanceTimersByTimeAsync(2000)
    await settle()
    expect(wrapper.find('nav').exists()).toBe(true)
    expect(wrapper.text()).not.toContain('Syno Reclaim is stopped')
    wrapper.unmount()
  }, 30_000)

  it('shows the stopped package, not an error with Retry, when it stops while the App loads, and loads once it runs', async () => {
    fakeGateRetryTimers()
    const { window, backend, client } = stoppableWindow({ stopAfter: 'session.get' })
    const { wrapper } = await mountApp({ backend, client })
    await elapseGateRetries()
    await settle()
    expect(wrapper.text()).toContain('Syno Reclaim is stopped')
    window.stopped = false
    await vi.advanceTimersByTimeAsync(2000)
    await settle()
    expect(wrapper.find('nav').exists()).toBe(true)
    expect(buttonWith(wrapper, 'Retry')).toBeUndefined()
    expect(wrapper.findComponent(LoadingArea).exists()).toBe(false)
    wrapper.unmount()
  }, 30_000)

  // With no finished job to reload them, the job poll that answers next
  // leaves the refused volumes as they were.
  it('loads the volumes again, not an error with Retry, when only their read was refused while the App loaded', async () => {
    fakeGateRetryTimers()
    const { backend, client } = stoppableWindow({ scenario: 'no_report', refuseOnce: 'volumes.list' })
    const { wrapper } = await mountApp({ backend, client })
    await elapseGateRetries()
    await settle()
    expect(wrapper.find('nav').exists()).toBe(true)
    expect(buttonWith(wrapper, 'Retry')).toBeUndefined()
    wrapper.unmount()
  }, 30_000)

  it('does not stay loading when the Overview is opened from Explore while stopped, and shows it once it runs', async () => {
    fakeGateRetryTimers()
    const { window, backend, client } = stoppableWindow()
    const { wrapper } = await mountApp({ backend, client })
    navigate('explore')
    await settle()
    window.stopped = true
    navigate('overview')
    await settle()
    await elapseGateRetries()
    await settle()
    expect(wrapper.text()).toContain('Syno Reclaim is stopped')
    window.stopped = false
    await vi.advanceTimersByTimeAsync(2000)
    // Recovery also starts the UI transition before its lazy page settles.
    await flushPromises()
    await vi.runOnlyPendingTimersAsync()
    await settle()
    expect(wrapper.find('nav').exists()).toBe(true)
    expect(wrapper.findComponent(LoadingArea).exists()).toBe(false)
    expect(wrapper.text()).toContain('Where the space went')
    wrapper.unmount()
  }, 30_000)
})

// The pages load while the executor's first status is read.
describe('App preloaded behind the executor approval', () => {
  beforeEach(() => {
    sessionStorage.clear()
    nav.page = 'overview'
    nav.newScanOpen = false
    resetAuthorization()
    Object.assign(session, {
      status: 'loading', user: null, toolVersion: null,
      cleanupEnabled: false, mutationsDisabled: false, error: null,
    })
    jobs.list = []
    jobs.error = null
  })

  type CapabilityStatus = Awaited<ReturnType<ExecutorSetup['status']>>

  /** An executor whose first status answers only when the test says so. */
  function pendingExecutor() {
    let answer!: (status: CapabilityStatus) => void
    const first = new Promise<CapabilityStatus>(resolve => { answer = resolve })
    const executor = {
      status: () => first,
      authorize: async () => ({ state: 'ready' }),
      control: async () => ({ state: 'ready' }),
    } satisfies ExecutorSetup
    return { executor, answer }
  }

  it('loads the session and volumes while the executor is checked', async () => {
    const { executor } = pendingExecutor()
    const { wrapper, calls } = await mountApp({ scenario: 'no_report', executor })
    expect(calls).toEqual(expect.arrayContaining(['session.get', 'jobs.list', 'volumes.list']))
    expect(wrapper.find('.executor-preloading').exists()).toBe(true)
    wrapper.unmount()
  })

  it('opens the new scan dialog only once the executor reads ready', async () => {
    const { executor, answer } = pendingExecutor()
    const { wrapper } = await mountApp({ scenario: 'no_report', executor })
    expect(nav.newScanOpen).toBe(false)
    answer({ state: 'ready' })
    await settle()
    expect(nav.newScanOpen).toBe(true)
    wrapper.unmount()
  })

  it('opens no new scan dialog when the executor needs approval', async () => {
    const { executor, answer } = pendingExecutor()
    const { wrapper } = await mountApp({ scenario: 'no_report', executor })
    answer({ state: 'needs-admin-approval' })
    await settle()
    expect(nav.newScanOpen).toBe(false)
    wrapper.unmount()
  })

  it('opens the running cleanup of the restored page only once the executor reads ready', async () => {
    nav.page = 'cleanupApply'
    const { executor, answer } = pendingExecutor()
    const { wrapper } = await mountApp({ scenario: 'job_recovery', executor })
    expect(wrapper.find('[role="dialog"]').exists()).toBe(false)
    answer({ state: 'ready' })
    await settle()
    expect(wrapper.find('[role="dialog"]').exists()).toBe(true)
    wrapper.unmount()
  })

  it('opens no cleanup dialog when the executor needs approval', async () => {
    nav.page = 'cleanupApply'
    const { executor, answer } = pendingExecutor()
    const { wrapper } = await mountApp({ scenario: 'job_recovery', executor })
    answer({ state: 'needs-admin-approval' })
    await settle()
    expect(wrapper.find('[role="dialog"]').exists()).toBe(false)
    wrapper.unmount()
  })

  it('opens the new scan dialog at once when the executor already reads ready', async () => {
    const { wrapper } = await mountApp({ scenario: 'no_report', executor: {
      status: async () => ({ state: 'ready' }),
      authorize: async () => ({ state: 'ready' }),
      control: async () => ({ state: 'ready' }),
    } })
    expect(nav.newScanOpen).toBe(true)
    wrapper.unmount()
  })
})

describe('App window height', () => {
  afterEach(() => vi.unstubAllGlobals())

  // The UI owns dialog bounds without publishing App sizing on the host document.
  it('leaves host document sizing to the UI while mounted', async () => {
    type Callback = (entries: { contentRect: { width: number; height: number } }[]) => void
    const observers: { callback: Callback; target?: Element }[] = []
    vi.stubGlobal('ResizeObserver', class {
      record: { callback: Callback; target?: Element }
      constructor(callback: Callback) { this.record = { callback }; observers.push(this.record) }
      observe(target: Element) { this.record.target = target }
      disconnect() {}
    })
    const { wrapper } = await mountApp()
    for (const observer of observers.filter(observer => observer.target?.hasAttribute('data-ui-app'))) {
      observer.callback([{ contentRect: { width: 1100, height: 680 } }])
    }
    await settle()
    try {
      expect(document.documentElement.style.getPropertyValue('--synoreclaim-window-height')).toBe('')
    } finally { wrapper.unmount() }
  })
})
