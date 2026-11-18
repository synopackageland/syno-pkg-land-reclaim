import { createDuplicatesMock } from '@/api/duplicatesFixture'
import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'
import { h } from 'vue'
import { AppRoot, ToastProvider } from '@synopackageland/ui'
import { AuthorizationGate, ExecutorSettings, ExecutorSetupKey } from '@synopackageland/sdk/vue'
import { ApiError, setBackend } from '@/api'
import { fixtureBackend } from '@/api/fixtures'
import { declineRootWork } from '@/authorizationFixture'
import type { Backend, Method, ParamsOf } from '@/api/methods'
import { session } from '@/stores/session'
import { preferences, readPreferences, SYSTEM_PARTITION_WARNING_KEY } from '@/stores/preferences'
import { configurePortal } from '@/portal'
import { createPortalAccess, type PortalAccess } from '@synopackageland/sdk'
import { loadVolumes, volumes } from '@/stores/volumes'
import type { GateContext } from '@/context'
import SettingsPage from './SettingsPage.vue'

type Overrides = Partial<Record<Method, (params: unknown) => unknown>>

function backendWith(overrides: Overrides): Backend {
  const base = fixtureBackend({ scenario: 'normal' })
  return ((method: Method, params: ParamsOf<Method>) => {
    const override = overrides[method]
    return override ? Promise.resolve(override(params)) : base(method, params as never)
  }) as Backend
}

const ready = { command: 'authorization', schema_version: 1, state: 'ready', reason: null } as const
const revoked = { ...ready, state: 'authorization_required', reason: 'grant_missing' } as const

/** The page lives inside the Gate, which owns the authorization row. */
function inGate(authorization: Partial<NonNullable<GateContext['authorization']>> = {}) {
  const ctx: GateContext = {
    locale: 'en',
    displayName: 'Syno Reclaim',
    reopen: () => () => {},
    authorization: { canManage: () => true, status: async () => ready, request: async () => ready, revoke: async () => revoked, ...authorization },
  }
  return { render: () => h(AppRoot, null, () => h(ToastProvider, null, () => h(AuthorizationGate, { ctx }, () => h(SettingsPage)))) }
}

async function mountPage(overrides: Overrides = {}, authorization: Parameters<typeof inGate>[0] = {}) {
  setBackend(backendWith(overrides))
  volumes.selected = '/volume1'
  await loadVolumes()
  const wrapper = mount(inGate(authorization), { attachTo: document.body })
  await flushPromises()
  return wrapper
}

const button = (wrapper: Awaited<ReturnType<typeof mountPage>>, text: string) =>
  wrapper.findAll('button').find(candidate => candidate.text() === text)!

const sectionsOf = (wrapper: VueWrapper) => wrapper.findAll('.ui-settings-page-body > section')

describe('SettingsPage', () => {
  beforeEach(() => {
    session.status = 'ready'
    session.toolVersion = '0.1.0'
    session.cleanupEnabled = true
    session.mutationsDisabled = false
  })

  afterEach(() => {
    vi.restoreAllMocks()
    vi.useRealTimers()
    configurePortal(undefined)
  })

  it('shows no Application Portal section outside DSM', async () => {
    const wrapper = await mountPage()
    expect(wrapper.text()).not.toContain('Application Portal')
  })

  // Below DSM 7.2 the root helper owns the entry; the SDK panel turns it on.
  it('turns the Application Portal entry on from Settings', async () => {
    let entry: 'registered' | 'unregistered' = 'unregistered'
    const status = async () => ({ state: 'inactive' as const, canManage: true, controlPanel: true, defaultAlias: 'synoreclaim', entry })
    const portal: PortalAccess = {
      messages: { title: 'Application Portal', entryOff: 'No entry yet', register: 'Enable entry', inactive: 'Not active', alias: 'Alias', apply: 'Apply', advanced: 'Advanced' },
      status,
      apply: status,
      disable: status,
      register: async () => {
        entry = 'registered'
        return status()
      },
      openControlPanel: () => {},
      mountPanel: () => ({ refresh: async () => {}, dispose: () => {} }),
    }
    configurePortal(portal)
    const wrapper = await mountPage()
    expect(wrapper.text()).toContain('No entry yet')

    await button(wrapper, 'Enable entry').trigger('click')
    await flushPromises()
    expect(entry).toBe('registered')
    expect(wrapper.text()).toContain('Not active')
  })

  // B4: on DSM 7.4 registering outlasts the panel's 5 s wait, and the helper
  // keeps its cleanup marker until it is done.
  it('never asks to remove the entry again while a slow register runs', async () => {
    vi.useFakeTimers()
    let entry = 'unregistered'
    configurePortal(createPortalAccess({
      appId: 'SYNO.SDS.SynoReclaim.Instance',
      defaultAlias: 'synoreclaim',
      locale: 'en',
      location: { protocol: 'https:', host: 'nas:5001' },
      isAdmin: () => true,
      entry: true,
      call: async method => {
        if (method === 'portal.status') return { state: 'inactive', entry, exit: 0 }
        entry = 'cleanup-required'
        await new Promise(resolve => setTimeout(resolve, 8000))
        entry = 'registered'
        return { state: 'inactive', entry, exit: 0 }
      },
      send: async api => (api === 'SYNO.API.Info' ? { 'SYNO.Core.AppPortal': { maxVersion: 2 } } : { id: 'SYNO.SDS.SynoReclaim.Instance', alias: '' }),
      launch: () => {},
    }))
    const wrapper = await mountPage()
    await button(wrapper, 'Enable entry').trigger('click')
    await vi.advanceTimersByTimeAsync(6000)
    expect(wrapper.text()).not.toContain('Remove the entry again')
    await vi.advanceTimersByTimeAsync(3000)
    expect(wrapper.text()).toContain('Not active')
  })

  it('registers the entry then applies the default alias with the SDK portal panel', async () => {
    let entry = 'unregistered'
    let alias = ''
    const calls: string[] = []
    configurePortal(createPortalAccess({
      appId: 'SYNO.SDS.SynoReclaim.Instance', defaultAlias: 'synoreclaim', locale: 'en',
      location: { protocol: 'https:', host: 'nas:5001' }, isAdmin: () => true, entry: true,
      call: async method => {
        calls.push(method)
        if (method === 'portal.register') entry = 'registered'
        return { state: 'inactive', entry, exit: 0 }
      },
      send: async (api, _method, _version, params) => {
        if (api === 'SYNO.API.Info') return { 'SYNO.Core.AppPortal': { maxVersion: 2 } }
        if (_method === 'set') {
          calls.push('alias.set')
          alias = String(params?.alias)
        }
        return { id: 'SYNO.SDS.SynoReclaim.Instance', alias }
      },
      launch: () => {},
    }))
    const wrapper = await mountPage()
    expect(wrapper.text()).toContain('Enable entry')
    await button(wrapper, 'Enable entry').trigger('click')
    await flushPromises()
    expect(calls.filter(method => method !== 'portal.status')).toEqual(['portal.register', 'alias.set'])
    expect(alias).toBe('synoreclaim')
    expect(wrapper.text()).toContain('https://nas:5001/synoreclaim/')
    expect(wrapper.text()).toContain('Remove entry')
    wrapper.unmount()
  })

  it('shows the authorization first and revokes it there after a confirmation', async () => {
    const revoke = vi.fn(async () => revoked)
    const wrapper = await mountPage({}, { revoke })
    const authorization = sectionsOf(wrapper)[0]!
    expect(authorization.text()).toContain('Authorized')
    await authorization.findAll('button').find(candidate => candidate.text() === 'Revoke authorization')!.trigger('click')
    await flushPromises()
    const confirm = [...document.querySelectorAll('button')].filter(candidate => candidate.textContent === 'Revoke authorization').at(-1)!
    confirm.click()
    await flushPromises()
    expect(revoke).toHaveBeenCalledOnce()
    wrapper.unmount()
  })

  it("shows the DSM window's executor approval first, with no revoke", async () => {
    setBackend(backendWith({}))
    const executor = { status: vi.fn(async () => ({ state: 'ready' as const })), authorize: vi.fn(), control: vi.fn() }
    const wrapper = mount(inGate(), { attachTo: document.body, global: { provide: { [ExecutorSetupKey]: executor } } })
    await flushPromises()
    const authorization = sectionsOf(wrapper)[0]!
    expect(authorization.find('[data-executor-status="ready"]').exists()).toBe(true)
    expect(wrapper.text()).not.toContain('Revoke authorization')
    wrapper.unmount()
  })

  it('tells the executor settings why the app needs system privileges', async () => {
    setBackend(backendWith({}))
    const executor = { status: vi.fn(async () => ({ state: 'needs-admin-approval' as const })), authorize: vi.fn(), control: vi.fn() }
    const wrapper = mount(inGate(), { attachTo: document.body, global: { provide: { [ExecutorSetupKey]: executor } } })
    await flushPromises()
    const settings = wrapper.findComponent(ExecutorSettings)
    expect(settings.props('purpose')).toBe(
      "It needs system privileges to measure the space used by every shared folder, snapshot and package, and to move only what you confirm into its shared folder's Recycle Bin, or delete it for good when that Recycle Bin is off or it is in no shared folder",
    )
    expect(wrapper.get('[data-executor-consent]').text()).toContain(settings.props('purpose'))
    wrapper.unmount()
  })

  // Cleanup stays allowed by default and is allowed again from the review;
  // the app follows the DSM language; Package Center shows the version.
  it('has no cleanup switch, language or version', async () => {
    const wrapper = await mountPage()
    // The one switch is the system partition warning.
    expect(wrapper.findAll('[role="switch"]')).toHaveLength(1)
    expect(wrapper.findAll('[role="checkbox"]')).toHaveLength(0)
    expect(sectionsOf(wrapper)[1]!.text()).toContain('Warn when the system partition is low on space')
    expect(wrapper.text()).not.toContain('Follows DSM')
    expect(wrapper.text()).not.toContain('0.1.0')
    wrapper.unmount()
  })

  it('lays the sections out directly in one column, not inside one card', async () => {
    const wrapper = await mountPage()
    expect(wrapper.find('.settings-card').exists()).toBe(false)
    const sections = sectionsOf(wrapper)
    expect(sections.map(section => section.find('div').text())).toEqual(['Authorization', 'System partition', 'Preferred shares for kept copies', 'Diagnostics'])
    // No section collapses; the preferred share picker is the one menu button.
    // No section collapses; the preferred share picker is the one dropdown.
    expect(wrapper.findAll('[aria-expanded]').map(node => node.attributes('aria-label'))).toEqual(['Add a shared folder'])
    wrapper.unmount()
  })

  it('explains each section under its title', async () => {
    const wrapper = await mountPage()
    const [, systemPartition, , diagnostics] = sectionsOf(wrapper)
    expect(systemPartition!.find('p').text()).toBe(
      'Overview warns when 900 MB or less is free on the system partition. With the warning off, you can still clean it up from its card on Overview',
    )
    expect(diagnostics!.find('p').text()).toContain('Paths are redacted')
    wrapper.unmount()
  })

  it('puts the support bundle download in the section footer beside its status', async () => {
    const wrapper = await mountPage({
      'support_bundle.get': () => {
        throw new ApiError('unknown')
      },
    })
    const diagnostics = sectionsOf(wrapper)[3]!
    const download = diagnostics.findAll('button').find(candidate => candidate.text() === 'Download support bundle')!
    // A section download is a tool action; capsules belong to form/dialog footers.
    expect(download.attributes('data-kind')).toBe('tool')
    await download.trigger('click')
    await flushPromises()
    expect(document.body.textContent).toContain('Could not create the support bundle')
    wrapper.unmount()
  })

  // One way to apply per page: the portal panel applies itself, so the page
  // has no Reset / Apply footer and no second main action.
  it('has no page footer beside the panels that apply themselves', async () => {
    const wrapper = await mountPage()
    expect(wrapper.find('.ui-settings-page-footer').exists()).toBe(false)
    expect(wrapper.findAll('button').filter(item => item.attributes('data-variant') === 'primary')).toHaveLength(0)
    wrapper.unmount()
  })

  it('turns the system partition warning off at once and keeps it off in this browser', async () => {
    localStorage.clear()
    readPreferences()
    const wrapper = await mountPage()
    const toggle = wrapper.get('[role="switch"]')
    // A settings row, like the authorization's Status row.
    expect(sectionsOf(wrapper)[1]!.text()).toContain('Low-space warning')
    expect(toggle.attributes('aria-checked')).toBe('true')
    await toggle.trigger('click')
    await flushPromises()
    expect(preferences.systemPartitionWarning).toBe(false)
    expect(localStorage.getItem(SYSTEM_PARTITION_WARNING_KEY)).toBe('false')
    expect(wrapper.get('[role="switch"]').attributes('aria-checked')).toBe('false')
    wrapper.unmount()

    readPreferences()
    const again = await mountPage()
    expect(again.get('[role="switch"]').attributes('aria-checked')).toBe('false')
    again.unmount()
    localStorage.clear()
    readPreferences()
  })

  it('changes nothing and downloads nothing when the authorization dialog is dismissed', async () => {
    const calls: string[] = []
    const wrapper = await mountPage({
      'support_bundle.get': () => { calls.push('support_bundle.get'); return {} },
    })
    const undo = declineRootWork()
    try {
      await button(wrapper, 'Download support bundle').trigger('click')
      await flushPromises()
      expect(calls).toEqual([])
      expect(wrapper.text()).not.toContain('Could not')
      expect(wrapper.text()).not.toContain('Downloaded')
    } finally {
      undo()
      wrapper.unmount()
    }
  })

  it('downloads the support bundle for the selected volume', async () => {
    const createObjectURL = vi.fn(() => 'blob:bundle')
    const revokeObjectURL = vi.fn()
    Object.assign(URL, { createObjectURL, revokeObjectURL })
    const click = vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(() => undefined)
    let asked: unknown
    const wrapper = await mountPage({
      'support_bundle.get': params => {
        asked = params
        return { schema_version: 1, command: 'support-bundle' }
      },
    })
    await button(wrapper, 'Download support bundle').trigger('click')
    await flushPromises()
    expect(asked).toEqual({ volume: '/volume1' })
    expect(createObjectURL).toHaveBeenCalledOnce()
    expect(click).toHaveBeenCalledOnce()
    expect(wrapper.text()).toContain('Downloaded')
    wrapper.unmount()
  })

  it('lists the preferred shares in order, saves a removal at once and starts the analysis', async () => {
    const mock = createDuplicatesMock({ preferred: ['photo', 'video'], analyzeMs: 60_000 })
    const calls: { method: string; params: unknown }[] = []
    const through = (method: 'duplicates.preferred_shares' | 'duplicates.analyze') => (params: unknown) => {
      calls.push({ method, params })
      return mock.answer(method, params)
    }
    const wrapper = await mountPage({ 'duplicates.preferred_shares': through('duplicates.preferred_shares'), 'duplicates.analyze': through('duplicates.analyze') })
    await vi.dynamicImportSettled()
    await flushPromises()
    const section = wrapper.get('[data-testid="preferred-shares"]')
    expect(section.findAll('tbody tr').map(row => row.text())).toEqual([expect.stringContaining('photo'), expect.stringContaining('video')])
    await section.get('button[aria-label="Remove photo"]').trigger('click')
    await vi.waitFor(() => expect(calls).toHaveLength(3))
    await flushPromises()
    expect(calls).toEqual([
      { method: 'duplicates.preferred_shares', params: { volume: '/volume1' } },
      { method: 'duplicates.preferred_shares', params: { volume: '/volume1', shares: ['video'] } },
      { method: 'duplicates.analyze', params: { volume: '/volume1' } },
    ])
    expect(section.findAll('tbody tr')).toHaveLength(1)
    wrapper.unmount()
  })
})
