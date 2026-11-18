import { enableAutoUnmount, flushPromises, mount } from '@vue/test-utils'
import { setBackend } from '@/api'
import { fixtureBackend } from '@/api/fixtures'
import App from '@/App.vue'
import { allowAuthorizationForTests } from '@/authorizationFixture'
import { nav, navigate, openJob } from '@/stores/nav'
import { session } from '@/stores/session'
import { AppLayout, AppSidebarUser } from '@synopackageland/ui'
import { AppShell, type AppShellPage } from '@synopackageland/sdk/vue'
import { SHELL_CONTEXT } from '@/context'
import { usageChange } from '@/usage/useUsageChange'

// Dispose watchers even if setup or an assertion fails.
enableAutoUnmount(afterEach)

async function mountApp(provide: Record<symbol, unknown> = {}, scenario = 'normal', stubs: Record<string, boolean> = {}) {
  setBackend(fixtureBackend({ scenario }))
  sessionStorage.clear()
  const wrapper = mount(App, { attachTo: document.body, global: { provide, stubs } })
  // The shell renders only after the session, jobs and volumes load; follow
  // their lazy imports instead of counting flushes (CI can be much slower).
  for (let i = 0; i < 50 && !wrapper.find('main').exists(); i++) {
    await flushPromises()
    await vi.dynamicImportSettled()
  }
  await flushPromises()
  return wrapper
}

async function settle() {
  await flushPromises()
  await vi.dynamicImportSettled()
  await flushPromises()
}

// Mounting App.vue needs the startup gate ready; the gate itself is tested separately.
allowAuthorizationForTests()

beforeEach(() => {
  // Module state: a previous test's ready session would render the shell on
  // the first paint, before this mount's own load finishes.
  session.status = 'loading'
})
afterEach(() => {
  nav.page = 'overview'
})

describe('AppShell pages', () => {
  // The SDK's shell scrolls the page under its header while the sidebar keeps its height.
  it("lays the app's pages out in the SDK shell at full width", async () => {
    const wrapper = await mountApp()
    const shell = wrapper.findComponent(AppShell)
    expect(shell.props('pageWidth')).toBe('fill')
    expect(shell.props('sidebarStorageKey')).toBe('synoreclaim')
    expect(shell.props('modelValue')).toBe('overview')
    wrapper.unmount()
  })

  // Explore's treemap, the Recycle Bin, Packages and History tables and the
  // Settings page fill the height under the header: their tables and sections scroll
  // inside, under a header that stays and above a footer that stays.
  it('lets Explore, the list pages and Settings fill the height under the header', async () => {
    const wrapper = await mountApp()
    const pages = wrapper.findComponent(AppShell).props('pages') as readonly AppShellPage[]
    expect(pages.filter(page => page.fill).map(page => page.id)).toEqual(['explore', 'duplicates', 'recycle', 'packages', 'history', 'settings'])
    wrapper.unmount()
  })

  it('lists Settings last in the sidebar and selects it on its page', async () => {
    const wrapper = await mountApp()
    const items = () => wrapper.find('aside').findAll('[data-nav-item]')
    expect(items().at(-1)!.text()).toBe('Settings')
    navigate('settings')
    await settle()
    expect(wrapper.find('aside [aria-current="page"]').text()).toBe('Settings')
    wrapper.unmount()
  })

  // The SDK lists a 'bottom' page after every other page, in a group of its own.
  it('places Settings in the sidebar\'s bottom group', async () => {
    const wrapper = await mountApp()
    const pages = wrapper.findComponent(AppShell).props('pages') as readonly AppShellPage[]
    expect(pages.filter(page => page.placement === 'bottom').map(page => page.id)).toEqual(['settings'])
    wrapper.unmount()
  })

  // A job's progress page is reached by following the job, not from the sidebar.
  it('keeps the progress pages out of the sidebar', async () => {
    const wrapper = await mountApp()
    navigate('scanning')
    await settle()
    expect(wrapper.find('h1.app-page-title').text()).toBe('Scanning')
    expect(wrapper.find('aside').findAll('[data-nav-item]').map(item => item.text())).not.toContain('Scanning')
    wrapper.unmount()
  })

  it('falls back to Overview for an unknown page', async () => {
    const wrapper = await mountApp()
    navigate('gone')
    await settle()
    expect(wrapper.find('h1.app-page-title').text()).toBe('Overview')
    wrapper.unmount()
  })
})

describe('AppShell header', () => {
  // The development fixture selector also sits in the header; only New scan matters here.
  const pageActions = (wrapper: Awaited<ReturnType<typeof mountApp>>) =>
    wrapper.find('[data-app-page-bar]').findAll('button')
      .map(button => button.attributes('aria-label') ?? button.text())
      .filter(name => name === 'New scan')

  it('keeps the page title accessible and offers New scan in the page bar', async () => {
    const wrapper = await mountApp()
    expect(wrapper.find('h1.app-page-title').text()).toBe('Overview')
    expect(wrapper.find('[data-app-page-bar]').text()).toContain('New scan')
    expect(pageActions(wrapper)).toEqual(['New scan'])
    wrapper.unmount()
  })

  // Pin the app-level behavior before replacing its local PageInfo with the SDK.
  it.each(['snapshots', 'packages'])("keeps %s notes in the page bar and toggles them", async (page) => {
    const wrapper = await mountApp({}, 'namespace_churn')
    navigate(page)
    await settle()
    const titleLine = wrapper.get('[data-app-page-bar]').element
    // The page's info button carries the scan status and the notes (SDK PageInfo).
    const button = titleLine.querySelector<HTMLButtonElement>('button.page-info')!
    expect(button).not.toBeNull()
    expect(button.querySelector('svg.lucide-info')).not.toBeNull()
    expect(document.body.querySelector('[role="tooltip"]')).toBeNull()
    button.click()
    await settle()
    const notes = [...document.body.querySelectorAll('[role="tooltip"] li')].map(item => item.textContent)
    expect(notes.length).toBeGreaterThan(0)
    button.click()
    await settle()
    expect(document.body.querySelector('[role="tooltip"]')).toBeNull()
    navigate('overview')
    await settle()
    // The notes leave with the page: Overview's info button, if any, lists none of them.
    const overviewButton = titleLine.querySelector<HTMLButtonElement>('button.page-info')
    overviewButton?.click()
    await settle()
    const shown = [...document.body.querySelectorAll('[role="tooltip"] li')].map(item => item.textContent)
    expect(shown.filter(note => notes.includes(note))).toEqual([])
    wrapper.unmount()
  })

  // An empty notices slot would still take the shell's notices row and its
  // 16px margin, pushing Overview and Explore below the other pages.
  it('leaves the notices row out when no job runs and usage has nothing to say', async () => {
    // Pin the empty state: on a slow runner, the background usage sampler's
    // first tick could repopulate the notices while lazy rendering settled.
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] })
    let wrapper: Awaited<ReturnType<typeof mountApp>> | undefined
    try {
      wrapper = await mountApp()
      usageChange.alerts = []
      usageChange.unavailable = []
      await settle()
      expect(wrapper.find('.app-notices').exists()).toBe(false)
    } finally {
      wrapper?.unmount()
      vi.useRealTimers()
    }
  })

  // Explore keeps the no-baseline hint behind its info button so its list starts higher.
  it('shows the no-baseline hint on Overview and leaves it to the info button on Explore', async () => {
    const wrapper = await mountApp({}, 'normal', { ExplorePage: true })
    usageChange.alerts = []
    usageChange.unavailable = ['/volume1']
    navigate('overview')
    await settle()
    expect(wrapper.find('.usage-hint').exists()).toBe(true)
    navigate('explore')
    await settle()
    expect(wrapper.find('.usage-hint').exists()).toBe(false)
    usageChange.unavailable = []
    wrapper.unmount()
  }, 30_000)

  // The ring said the scan paused while the header still said Scanning.
  it('titles the Scanning page Scan paused while its scan is paused', async () => {
    vi.useFakeTimers({ toFake: ['Date'] })
    vi.setSystemTime(new Date('2026-09-30T01:04:20Z'))
    setBackend(fixtureBackend({ scenario: 'paused' }))
    nav.page = 'scanning'
    const wrapper = mount(App, { attachTo: document.body })
    for (let i = 0; i < 50 && !wrapper.find('main').exists(); i++) await settle()
    await settle()
    expect(wrapper.find('h1.app-page-title').text()).toBe('Scan paused')
    wrapper.unmount()
    vi.useRealTimers()
  })

  // Settings starts no scan; the shell's one header names it.
  it('keeps Settings named without a duplicate header or New scan', async () => {
    const wrapper = await mountApp()
    navigate('settings')
    await settle()
    expect(wrapper.findAll('h1.app-page-title')).toHaveLength(1)
    expect(wrapper.find('[data-ui-page-header]').exists()).toBe(false)
    expect(wrapper.find('h1.app-page-title').text()).toBe('Settings')
    expect(wrapper.findAll('button').map(button => button.text())).not.toContain('New scan')
    wrapper.unmount()
  })
})

// A Portal/standalone page owns the browser window; a DSM desktop window does not.
describe('AppShell presentation', () => {
  it('keeps the dense DSM window layout inside a desktop window', async () => {
    const wrapper = await mountApp({ [SHELL_CONTEXT]: { fullPage: false, displayName: 'Syno Reclaim' } })
    expect(wrapper.findComponent(AppLayout).props('wideLayout')).toBe('window')
    wrapper.unmount()
  })

  // A full page has no DSM title bar: the sidebar names the app and the account.
  it("takes the web layout, the launch's brand and its account on a full page", async () => {
    // A 1440px page: jsdom measures no width, so report one.
    vi.stubGlobal('ResizeObserver', class {
      constructor(private callback: ResizeObserverCallback) {}
      observe() { this.callback([{ contentRect: { width: 1440 } } as ResizeObserverEntry], this as unknown as ResizeObserver) }
      disconnect() {}
      unobserve() {}
    })
    const wrapper = await mountApp({ [SHELL_CONTEXT]: {
      fullPage: true,
      displayName: 'Syno Reclaim',
      appIcon: 'webman/3rdparty/SynoReclaim/images/icon_64.png',
      user: { name: 'admin', isAdmin: true },
    } })
    const layout = wrapper.findComponent(AppLayout)
    expect(layout.props('wideLayout')).toBe('web')
    expect(layout.props('brandName')).toBe('Syno Reclaim')
    expect(layout.props('brandIcon')).toBe('webman/3rdparty/SynoReclaim/images/icon_64.png')
    expect(layout.attributes('data-ui-layout-mode')).toBe('web')
    expect(wrapper.findComponent(AppSidebarUser).props('name')).toBe('admin')
    wrapper.unmount()
    vi.unstubAllGlobals()
  })
})

it('lists each running job once in one status block and hides the followed walk on its progress view', async () => {
  const wrapper = await mountApp()
  const { jobs } = await import('@/stores/jobs')
  const source = (await fixtureBackend({ scenario: 'scanning' })('jobs.list', {})).jobs[0]!
  const report = { ...source, job_id: 'report-1', kind: 'rootfs-report' as const, phase: 'report' as const, progress: undefined, elapsed_s: 16 }
  const listed = fixtureBackend({ scenario: 'normal' })
  setBackend(async (method, params) => method === 'jobs.list'
    ? { ...(await listed(method, params)), jobs: [source, report] } as never
    : listed(method, params))
  try {
    navigate('overview')
    jobs.list = [source, report]
    await settle()
    expect(wrapper.findAll('.status-list')).toHaveLength(1)
    expect(wrapper.findAll('.status-list .job-strip')).toHaveLength(2)
    openJob(report)
    for (let i = 0; i < 20 && !wrapper.text().includes('Scanning: System partition'); i++) await settle()
    expect(nav.page).toBe('scanning')
    expect(wrapper.findAll('.job-strip')).toHaveLength(0)
    expect(wrapper.text()).toContain('Scanning: System partition')
    expect(wrapper.text()).toContain('Entry counts are not available for this scan')
    expect(wrapper.text()).not.toContain('Pause')
  } finally { wrapper.unmount(); jobs.list = [] }
}, 30_000)
