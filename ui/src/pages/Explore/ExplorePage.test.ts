import { enableAutoUnmount, flushPromises, mount } from '@vue/test-utils'
import { Callout, DataGrid, Tooltip, type DataGridColumn } from '@synopackageland/ui'
import { fixtureBackend } from '@/api/fixtures'
import { apiErrorFromCgi } from '@/api/errors'
import { ApiError, setBackend } from '@/api'
import type { Backend, Method, ParamsOf, ResultOf } from '@/api/methods'
import App from '@/App.vue'
// App loads pages lazily. Load the subject with the test module so Vite's
// first-page transformation is not charged to whichever interaction runs first.
import './ExplorePage.vue'
import { allowAuthorizationForTests } from '@/authorizationFixture'
import { nav, openInExplore, takePackageRequest } from '@/stores/nav'
import { session } from '@/stores/session'
import { useBasket } from '@/stores/basket'
import { answersVersion, invalidateAnswers } from '@/stores/answers'
import { clearAnswers } from '@/api/answerCache'
import { volumes } from '@/stores/volumes'
import { openPageInfo, openPageStatus, pageStatusText } from '@/components/pageInfoTesting'
import ScanBadge from '@/components/ScanBadge.vue'
import { formatBytes } from '@/i18n'
import { usageChange } from '@/usage/useUsageChange'
import type { ExploreEntry } from './treemap'

/** The visible breadcrumb crumbs; the current level is plain text, the others are buttons. */
const CRUMBS = '[aria-label="Path"] > ol:not([aria-hidden]) :is(button, [aria-current="page"])'

/** A promise a test settles by hand, to hold one navigation open. */
function deferred<T>() {
  let resolve!: (value: T) => void
  const promise = new Promise<T>(settle => { resolve = settle })
  return { promise, resolve }
}

/**
 * Keeps the first few rows of each top.list answer. The scenarios that only
 * change a report's status fall back to the 200-entry volume fixture, and
 * rendering its rows and tiles in jsdom took most of a 5 s test timeout on
 * a loaded gate host; the status, notes and callouts do not need them.
 */
function fewRows(backend: Backend): Backend {
  return async <M extends Method>(method: M, params: ParamsOf<M>): Promise<ResultOf<M>> => {
    const answer = await backend(method, params)
    if (method !== 'top.list') return answer
    const list = answer as ResultOf<'top.list'>
    return { ...list, results: list.results.map(result => ({ ...result, entries: result.entries.slice(0, 5) })) } as ResultOf<M>
  }
}

async function openShare04(wrapper: Awaited<ReturnType<typeof mountExplore>>) {
  await wrapper.find('button[aria-label="/volume1/share04"]').trigger('click')
  await flushPromises()
  await vi.dynamicImportSettled()
  await flushPromises()
}

function rowBox(wrapper: Awaited<ReturnType<typeof mountExplore>>, path: string) {
  return wrapper.findAll('tbody tr').find(row => row.find(`button[aria-label="${path}"]`).exists())?.find('td:first-child button[role="checkbox"]')
}

async function mountExplore(scenario = 'interaction', wrap: (backend: Backend) => Backend = backend => backend, stubs: string[] = []) {
  sessionStorage.clear()
  nav.page = 'explore'
  nav.newScanOpen = false
  session.status = 'loading'
  volumes.document = null
  volumes.selected = null
  useBasket('/volume1').clear()
  const scenarioBackend = fixtureBackend({ scenario })
  const normalBackend = fixtureBackend()
  const backend: Backend = scenario === 'error'
    ? (method, params) => method === 'session.get' ? normalBackend(method, params) : scenarioBackend(method, params)
    : scenarioBackend
  setBackend(wrap(backend))
  const wrapper = mount(App, { attachTo: document.body, global: { stubs } })
  await flushPromises()
  await vi.dynamicImportSettled()
  await flushPromises()
  return wrapper
}

// Mounting App.vue needs the startup gate ready; the gate itself is tested separately.
allowAuthorizationForTests()
// A timeout or failed assertion must still stop the App's shared-store watches
// and polling before the next test resets those stores. Clearing the DOM alone
// does not dispose Vue effects.
enableAutoUnmount(afterEach)


describe('Explore page', () => {
  // A teleport target or a detached app left behind by one test would take the
  // next test's toolbar chip; start every test with an empty body.
  beforeEach(() => {
    document.body.innerHTML = ''
  })
  it('unmounts the App after an interrupted setup, before another test changes shared stores', () => {
    const wrapper = mount(App, { attachTo: document.body })
    // A rejected assertion/setup must not bypass disposal of the App's watches
    // and polling. This check runs after afterEach, as a failed test would too.
    onTestFinished(() => expect(wrapper.vm.$.isUnmounted).toBe(true))
    // Leave before awaiting startup, as an error in mountExplore would do.
    // There is deliberately no manual unmount on this path.
    expect(wrapper.vm.$.isUnmounted).toBe(false)
  })

  it.each(['unknown_small', 'namespace_churn'])('suppresses %s without a separate note', async scenario => {
    const wrapper = await mountExplore(scenario, fewRows)
    expect(wrapper.find('[data-testid="unattributed-note"]').exists()).toBe(false)
    expect(wrapper.find('[data-testid="namespace-churn-note"]').exists()).toBe(false)
    expect(wrapper.findAllComponents(Callout).some(callout => callout.props('tone') === 'warning')).toBe(false)
    expect(document.body.textContent).not.toContain('Not fully read')
    wrapper.unmount()
  })

  it.each(['coverage_gap', 'namespace_churn_many', 'partial'])('keeps %s coverage honest in compact status details', async scenario => {
    const wrapper = await mountExplore(scenario, fewRows)
    expect(wrapper.findAllComponents(Callout).some(callout => callout.props('tone') === 'warning')).toBe(false)
    expect(pageStatusText()).toContain('Not fully read')
    expect((await openPageStatus()).text).toContain('Some data could not be read')
    wrapper.unmount()
  })

  it('keeps coverage compact when the answer is complete but the report is partial', async () => {
    const wrapper = await mountExplore('coverage_gap', backend => fewRows(async (method, params) => {
      const answer = await backend(method, params)
      return method === 'top.list' ? { ...answer, completion: 'complete' } as ResultOf<typeof method> : answer
    }))
    expect(wrapper.findAllComponents(Callout).some(callout => callout.props('tone') === 'warning')).toBe(false)
    expect(String(wrapper.getComponent(ScanBadge).props('details')).includes('Some data could not be read')).toBe(true)
    wrapper.unmount()
  })

  it('keeps above-threshold unknown details in the status without a separate note', async () => {
    const wrapper = await mountExplore('unknown_large', fewRows)
    expect(wrapper.find('[data-testid="unattributed-note"]').exists()).toBe(false)
    expect(String(wrapper.getComponent(ScanBadge).props('details')).includes('could not be attributed and is not counted as reclaimable')).toBe(true)
    expect(document.body.textContent).not.toContain('Not fully read')
    wrapper.unmount()
  })

  it('retains compact partial status for a legacy one-entry report without completeness evidence', async () => {
    const wrapper = await mountExplore('namespace_churn', backend => fewRows(async (method, params) => {
      const answer = await backend(method, params)
      if (method === 'top.list') {
        const report = (answer as ResultOf<'top.list'>).results[0].report
        delete report.attribution!.namespace_churn
        delete report.coverage.unreadable_roots
      }
      return answer
    }))
    expect(pageStatusText()).toContain('Not fully read')
    expect(wrapper.findAllComponents(Callout).some(callout => callout.props('tone') === 'warning')).toBe(false)
    expect(String(wrapper.getComponent(ScanBadge).props('details')).includes('Some data could not be read')).toBe(true)
    wrapper.unmount()
  })

  it.each(['failed', 'cancelled', 'paused', 'runtime_limit', 'resource_limit', 'interrupted'])('keeps %s scan failures prominent', async completion => {
    const wrapper = await mountExplore('unknown_small', backend => fewRows(async (method, params) => {
      const answer = await backend(method, params)
      return method === 'top.list' ? { ...answer, completion } as ResultOf<typeof method> : answer
    }))
    expect(wrapper.findAllComponents(Callout).some(callout => ['warning', 'danger'].includes(callout.props('tone') ?? ''))).toBe(true)
    wrapper.unmount()
  })

  it('keeps report errors prominent even when the unknown bytes are small', async () => {
    const wrapper = await mountExplore('unknown_small', backend => fewRows(async (method, params) => {
      const answer = await backend(method, params)
      return method === 'top.list'
        ? { ...answer, errors: [{ code: 'unreadable', message: 'read failed' }] } as ResultOf<typeof method>
        : answer
    }))
    expect(wrapper.findAllComponents(Callout).some(callout => ['warning', 'danger'].includes(callout.props('tone') ?? ''))).toBe(true)
    wrapper.unmount()
  })

  it('keeps included rows in the list with a hint but excludes their treemap tiles', async () => {
    const wrapper = await mountExplore('interaction', backend => async (method, params) => {
      const answer = await backend(method, params)
      if (method !== 'top.list') return answer
      const top = answer as ResultOf<'top.list'>
      const parent = { ...structuredClone(top.results[0].entries[0]),
        path: '/volume1/share04', kind: 'dir' as const, service: null }
      top.results[0].entries.push(parent)
      top.results[0].entries.push({
        ...structuredClone(parent), path: '/volume1/share04/#recycle',
        within: { path: parent.path },
      } as typeof parent)
      return top as ResultOf<typeof method>
    })
    const row = wrapper.find('button[aria-label="/volume1/share04/#recycle"]')
    expect(row.exists()).toBe(true)
    expect(row.text()).toContain('Included in share04')
    expect(wrapper.findAll('svg[role="group"] g[role="button"]').some(tile =>
      tile.attributes('aria-label')?.startsWith('#recycle:'),
    )).toBe(false)
    wrapper.unmount()
  })

  it('opens the share reports of a volume that only had share scans', async () => {
    const wrapper = await mountExplore('share_scan')

    expect(wrapper.text()).not.toContain('Not scanned yet')
    expect(wrapper.text()).toContain('No whole-volume scan yet')
    const share = wrapper.find('button[aria-label="/volume1/share04"]')
    expect(share.exists()).toBe(true)
    await share.trigger('click')
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    expect(wrapper.find('button[aria-label="/volume1/share04/folder0005"]').exists()).toBe(true)
    wrapper.unmount()
  })

  describe('which report answers the volume root', () => {
    /** share04 scanned on its own after the volume scan: its root answers from that newer share report. */
    function newerShareScan(seen: ParamsOf<'top.list'>[] = []) {
      const shareScan = fixtureBackend({ scenario: 'share_scan' })
      return (backend: Backend): Backend => async (method, params) => {
        if (method === 'top.list') seen.push(params as ParamsOf<'top.list'>)
        const top = params as ParamsOf<'top.list'>
        if (method === 'top.list' && top.target === '/volume1/share04' && !top.report) return shareScan(method, params)
        return backend(method, params)
      }
    }
    const shareRow = (wrapper: Awaited<ReturnType<typeof mountExplore>>) =>
      wrapper.findAll('tbody tr').find(row => row.find('button[aria-label="/volume1/share04"]').exists())!

    it('answers a share scanned after the volume scan from its own report, and says so', async () => {
      const wrapper = await mountExplore('interaction', newerShareScan())

      const row = shareRow(wrapper)
      expect(row.findAll('td').map(cell => cell.text()).slice(2, 4)).toEqual(['2.6 MB', '0 B'])
      expect(row.text()).toContain('Shared folder scan')
      const chip = pageStatusText() ?? ''
      expect(chip).toContain('Whole volume + 1 newer shared folder')
      wrapper.unmount()
    })

    it('switches to the whole-volume report, which answers every share as it saw it', async () => {
      const seen: ParamsOf<'top.list'>[] = []
      const wrapper = await mountExplore('interaction', newerShareScan(seen))
      const picker = wrapper.find('[data-testid="explore-report"]')
      expect(picker.exists()).toBe(true)
      expect(picker.text()).toContain('Newest scan of each folder')

      const volumeReport = wrapper.findComponent({ name: 'ReportPicker' })
      volumeReport.vm.$emit('update:modelValue', 'fixture-job0010')
      await flushPromises()
      await vi.dynamicImportSettled()
      await flushPromises()

      expect(seen.at(-1)).toMatchObject({ target: '/volume1', report: 'fixture-job0010' })
      const row = shareRow(wrapper)
      expect(row.text()).not.toContain('Shared folder scan')
      expect(row.findAll('td').map(cell => cell.text())[2]).not.toBe('2.6 MB')
      expect(pageStatusText()).toContain('Whole volume')
      expect(pageStatusText()).not.toContain('newer shared folder')
      wrapper.unmount()
    })
  })

  it('reads the answer again after a cleanup invalidates it', async () => {
    let topReads = 0
    const wrapper = await mountExplore('interaction', backend => async (method, params) => {
      if (method === 'top.list') topReads += 1
      return backend(method, params)
    })
    const before = topReads

    invalidateAnswers()
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()

    expect(answersVersion.value).toBeGreaterThan(0)
    expect(topReads).toBeGreaterThan(before)
    wrapper.unmount()
  })

  it('shows the parent level from the cache instead of asking again', async () => {
    let topReads = 0
    const wrapper = await mountExplore('interaction', backend => async (method, params) => {
      if (method === 'top.list') topReads += 1
      return backend(method, params)
    })
    const atRoot = topReads

    await wrapper.find('button[aria-label="/volume1/share04"]').trigger('click')
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    expect(topReads).toBe(atRoot + 1)

    await wrapper.find('[aria-label="Path"] > ol:not([aria-hidden]) button[title="volume1"]').trigger('click')
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    expect(wrapper.find('button[aria-label="/volume1/share04"]').exists()).toBe(true)
    expect(topReads).toBe(atRoot + 1)
    wrapper.unmount()
  })

  it('keeps the previous level while the next one loads', async () => {
    const pendingShare = deferred<ResultOf<'top.list'>>()
    const wrapper = await mountExplore('interaction', backend => async (method, params) => {
      if (method === 'top.list' && (params as ParamsOf<'top.list'>).target === '/volume1/share04') return pendingShare.promise
      return backend(method, params)
    })

    await wrapper.find('button[aria-label="/volume1/share04"]').trigger('click')
    await flushPromises()

    // The share level is still loading, so the volume level's treemap stays on screen.
    expect(wrapper.findAll('svg[role="group"] g[role="button"]').some(tile =>
      tile.attributes('aria-label')?.startsWith('Synology Drive Server:'),
    )).toBe(true)
    wrapper.unmount()
  })

  it('never lets a superseded navigation change the screen', async () => {
    const scenario = fixtureBackend({ scenario: 'interaction' })
    const pendingShare = deferred<ResultOf<'top.list'>>()
    const wrapper = await mountExplore('interaction', backend => async (method, params) => {
      if (method === 'top.list' && (params as ParamsOf<'top.list'>).target === '/volume1/share04') return pendingShare.promise
      return backend(method, params)
    })

    await wrapper.find('button[aria-label="/volume1/share04"]').trigger('click')
    await flushPromises()
    await wrapper.find('[aria-label="Path"] > ol:not([aria-hidden]) button[title="volume1"]').trigger('click')
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    expect(wrapper.text()).toContain('Synology Drive Server')

    pendingShare.resolve(await scenario('top.list', { target: '/volume1/share04', kind: 'all', by: 'reclaimable', limit: 200 }))
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()

    expect(wrapper.text()).toContain('Synology Drive Server')
    expect(wrapper.text()).not.toContain('folder0005')
    wrapper.unmount()
  })

  it('keeps the breadcrumb to the shown level when sibling folders are clicked fast', async () => {
    const scenario = fixtureBackend({ scenario: 'share_scan' })
    const held = new Map<string, ReturnType<typeof deferred<ResultOf<'top.list'>>>>()
    const hold = (path: string) => {
      const waiting = deferred<ResultOf<'top.list'>>()
      held.set(path, waiting)
      return waiting.promise
    }
    const wrapper = await mountExplore('share_scan', backend => async (method, params) => {
      if (method === 'top.list') {
        const target = (params as ParamsOf<'top.list'>).target
        const path = typeof target === 'string' ? target : target.path
        if (path.startsWith('/volume1/share04/folder')) return hold(path) as never
      }
      return backend(method, params)
    })
    const crumbText = () => wrapper.findAll(CRUMBS).map(crumb => crumb.text())

    await wrapper.find('button[aria-label="/volume1/share04"]').trigger('click')
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    expect(wrapper.find('button[aria-label="/volume1/share04/folder0005"]').exists()).toBe(true)
    expect(crumbText()).toEqual(['volume1', 'share04'])

    for (const name of ['folder0005', 'folder0004', 'folder0003']) {
      await wrapper.find(`button[aria-label="/volume1/share04/${name}"]`).trigger('click')
    }
    // The last click replaces the pending ones: no superseded folder leaves a crumb.
    expect(crumbText()).toEqual(['volume1', 'share04', 'folder0003'])

    for (const name of ['folder0005', 'folder0004', 'folder0003']) {
      const path = `/volume1/share04/${name}`
      const answer = structuredClone(await scenario('top.list', { target: '/volume1/share04', kind: 'all', by: 'reclaimable', limit: 200 })) as ResultOf<'top.list'>
      answer.results[0].target = { ...answer.results[0].target, path }
      answer.results[0].entries = []
      held.get(path)!.resolve(answer)
    }
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()

    expect(crumbText()).toEqual(['volume1', 'share04', 'folder0003'])
    wrapper.unmount()
  })

  it('keeps the full chain when a row is opened right after an ancestor crumb', async () => {
    const scenario = fixtureBackend({ scenario: 'share_scan' })
    const pendingRoot = deferred<ResultOf<'top.list'>>()
    let holdRoot = false
    const wrapper = await mountExplore('share_scan', backend => async (method, params) => {
      if (holdRoot && method === 'top.list' && (params as ParamsOf<'top.list'>).target === '/volume1') return pendingRoot.promise as never
      return backend(method, params)
    })
    const crumbText = () => wrapper.findAll(CRUMBS).map(crumb => crumb.text())

    await wrapper.find('button[aria-label="/volume1/share04"]').trigger('click')
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    expect(crumbText()).toEqual(['volume1', 'share04'])

    // The ancestor answer is not in the cache, so the crumb navigation stays in flight.
    clearAnswers()
    holdRoot = true
    await wrapper.find('[aria-label="Path"] > ol:not([aria-hidden]) button[title="volume1"]').trigger('click')
    // The breadcrumb is hidden at the volume root, so only rows carry the level.
    expect(crumbText()).toEqual([])
    await wrapper.find('button[aria-label="/volume1/share04/folder0004"]').trigger('click')
    expect(crumbText()).toEqual(['volume1', 'share04', 'folder0004'])

    // The superseded root answer must not shorten the chain when it lands.
    pendingRoot.resolve(await scenario('top.list', { target: '/volume1', kind: 'all', by: 'reclaimable', limit: 200 }))
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    expect(crumbText()).toEqual(['volume1', 'share04', 'folder0004'])
    wrapper.unmount()
  })

  it('loads share totals one page at a time instead of one call per share', async () => {
    const requested: string[] = []
    const manyShares = (backend: Backend): Backend => async (method, params) => {
      if (method === 'top.list' && (params as ParamsOf<'top.list'>).limit === 1) {
        requested.push(String((params as ParamsOf<'top.list'>).target))
      }
      const answer = await backend(method, params)
      if (method !== 'history.list') return answer
      const doc = structuredClone(answer) as ResultOf<'history.list'>
      const template = doc.scans.find(scan => !scan.volume && scan.report != null && scan.outcome === 'completed')!
      doc.scans = Array.from({ length: 60 }, (_, index) => ({
        ...structuredClone(template),
        targets: [{ path: `/volume1/share${String(index).padStart(2, '0')}` }],
      }))
      return doc as ResultOf<'history.list'>
    }
    // Pagination exercises the real page/backend; grid and treemap rendering have separate tests.
    const wrapper = await mountExplore('share_scan', manyShares, ['ReclaimGrid', 'Treemap'])
    expect(requested).toHaveLength(50)
    expect(new Set(requested).size).toBe(50)
    const more = wrapper.findAll('button').find(button => button.text() === 'Load sizes for 10 more shared folders')!
    expect(more.exists()).toBe(true)
    await more.trigger('click')
    await flushPromises()
    expect(requested).toHaveLength(60)
    expect(new Set(requested).size).toBe(60)
    expect(wrapper.text()).not.toContain('Load sizes for 10 more shared folders')
    wrapper.unmount()
  })

  it('shows each share report\'s root totals at the volume root', async () => {
    const wrapper = await mountExplore('share_scan')

    const row = wrapper.findAll('tbody tr').find(candidate => candidate.find('button[aria-label="/volume1/share04"]').exists())
    const cells = row!.findAll('td').map(cell => cell.text())
    expect(cells.slice(2, 5)).toEqual(['2.6 MB', '0 B', 'snapshot 2.6 MB'])
    expect(wrapper.findAll('svg[role="group"] g[role="button"]').some(tile =>
      tile.attributes('aria-label')?.startsWith('share04:'),
    )).toBe(true)
    expect(pageStatusText()).toContain('No changes seen')
    wrapper.unmount()
  })

  // Wrapped names made rows two or three lines high and cut rows in
  // half at the list's edge. Every line is one line again, cut with an
  // ellipsis whose full text the grid's tooltip shows, so rows keep one height.
  // With its full columns (a maximized 1440 px window, B14) the list has the
  // room to show every line whole, so they wrap instead of being cut; the
  // compact list keeps one line each with the full text in the tooltip.
  it("wraps a row's name and context, what holds it and where to manage it when the list has its full columns", async () => {
    const wrapper = await mountExplore()
    const headers = wrapper.findAll('thead th').map(th => th.text())
    const where = headers.findIndex(header => header.startsWith('Where to manage'))
    const rows = wrapper.findAll('tbody tr').filter(row => row.findAll('td').length === headers.length)
    const contexts = rows.flatMap(row => row.findAll('td')[headers.findIndex(header => header.startsWith('Name'))].findAll('[data-ui-cell-primary], [data-ui-cell-secondary]'))
    const held = headers.findIndex(header => header.startsWith('Held by'))
    const places = rows.flatMap(row => [where, held].flatMap(column => row.findAll('td')[column].findAll('[data-ui-cell-primary], [data-ui-cell-secondary]')))
    expect(contexts.length).toBeGreaterThan(0)
    expect(places.length).toBeGreaterThan(0)
    for (const line of [...contexts, ...places]) expect(line.element.closest('[data-layout]')?.hasAttribute('data-wrap')).toBe(true)
    wrapper.unmount()
  })

  it('names a share-scanned path a shared folder only when volumes.list lists it as one', async () => {
    const context = (wrapper: Awaited<ReturnType<typeof mountExplore>>) =>
      wrapper.find('button[aria-label="/volume1/share04"] [data-ui-cell-secondary]').text()
    const shareWrapper = await mountExplore('share_scan')
    expect(context(shareWrapper)).toBe('Shared Folder')
    shareWrapper.unmount()

    const notAShare = (backend: Backend): Backend => async (method, params) => {
      const answer = await backend(method, params)
      if (method !== 'volumes.list') return answer
      const list = answer as ResultOf<'volumes.list'>
      return {
        ...list,
        volumes: list.volumes.map(volume => ({ ...volume, shares: volume.shares.filter(share => share.path.path !== '/volume1/share04') })),
      } as never
    }
    const folderWrapper = await mountExplore('share_scan', notAShare)
    expect(context(folderWrapper)).toBe('Folder')
    folderWrapper.unmount()
  })

  it('shows a centred note instead of an empty treemap when nothing here has a size', async () => {
    const withoutShareAnswers = (backend: Backend): Backend => (method, params) =>
      method === 'top.list' && (params as ParamsOf<'top.list'>).target !== '/volume1'
        ? Promise.reject(new ApiError('exec_failed'))
        : backend(method, params)
    const wrapper = await mountExplore('share_scan', withoutShareAnswers)

    expect(wrapper.find('button[aria-label="/volume1/share04"]').exists()).toBe(true)
    expect(wrapper.find('svg[role="group"]').exists()).toBe(false)
    expect(wrapper.get('[data-testid="explore-treemap-empty"]').text()).toContain('No sizes to show here yet')
    wrapper.unmount()
  })

  it('says a system operation failed, with its code, when the volume level fails as root', async () => {
    const rootFailed = (backend: Backend): Backend => (method, params) =>
      method === 'top.list' ? Promise.reject(apiErrorFromCgi('ROOT_FAILED')) : backend(method, params)
    const wrapper = await mountExplore('normal', rootFailed)
    expect(wrapper.text()).toContain('A system operation failed (error code: ROOT_FAILED)')
    expect(wrapper.text()).not.toContain('Package Center')
    expect(wrapper.findAll('button').some(button => button.text() === 'Retry')).toBe(true)
    wrapper.unmount()
  })

  // A level like a log folder: one large file beside small subfolders and many
  // small files. Every folder stays reachable on the map, and a folder tile
  // opens the folder while a file tile opens its details (regression, 2026-10-22).
  describe('a level of one large file beside small folders and files', () => {
    // Keep all 44 entries and the real page, Treemap and navigation. These two
    // cases exercise the map, not 44 rows of unrelated grid controls/tooltips;
    // rendering both made jsdom DOM construction dominate the 5 s budget.
    const stubs = ['ReclaimGrid']
    const LEVEL = '/volume1/share62/logs'
    const folders = ['archive', 'cores', 'nginx'].map(name => `${LEVEL}/${name}`)
    function mixedLevel(backend: Backend, targets: string[]): Backend {
      return async <M extends Method>(method: M, params: ParamsOf<M>): Promise<ResultOf<M>> => {
        if (method !== 'top.list') return backend(method, params)
        const raw = (params as { target: string | { path: string } }).target
        const target = typeof raw === 'string' ? raw : raw.path
        targets.push(target)
        const answer = structuredClone(await backend(method, { ...params, target: '/volume1/share62' } as ParamsOf<M>)) as ResultOf<'top.list'>
        const template = answer.results[0].entries.find(entry => entry.kind === 'file')!
        const row = (path: string, kind: 'dir' | 'file', physical: number) => ({
          ...structuredClone(template), path, path_hex: undefined, kind, service: null, flags: [], held_by: [], within: undefined, blockers: [], nlink: 1,
          cleanup_eligible: true, metrics: { ...template.metrics, physical, reclaimable: physical, logical: physical, unknown: 0, pinned_unreferenced: 0 },
        })
        answer.results[0].target = { ...answer.results[0].target, path: target, path_hex: undefined }
        answer.results[0].entries = target === LEVEL
          ? [
              row(`${LEVEL}/daemon.log_0`, 'file', 5 * 1024 * 1024),
              ...folders.map((path, index) => row(path, 'dir', (index + 1) * 4096)),
              ...Array.from({ length: 40 }, (_, index) => row(`${LEVEL}/small${index}.log`, 'file', 4096)),
            ]
          : [row(`${target}/inner.log`, 'file', 8192)]
        return answer as ResultOf<M>
      }
    }

    function tile(wrapper: Awaited<ReturnType<typeof mountExplore>>, name: string) {
      return wrapper.findAll('svg[role="group"] g[role="button"]').find(item => item.attributes('aria-label')?.startsWith(`${name}:`))
    }

    it('shows the file and every folder of the level, small ones grouped in a tile that opens them', async () => {
      openInExplore({ path: LEVEL })
      const wrapper = await mountExplore('normal', backend => mixedLevel(backend, []), stubs)
      expect(tile(wrapper, 'daemon.log_0')?.attributes('data-kind')).toBe('file')
      const group = tile(wrapper, '3 more folders')
      expect(group?.attributes('data-kind')).toBe('group')
      await group!.trigger('click')
      for (const path of folders) expect(tile(wrapper, path.split('/').at(-1)!)?.attributes('data-kind')).toBe('folder')
      wrapper.unmount()
    })

    it('opens a folder tile as the next level and a plain file tile in File Station', async () => {
      const calls: { id: string; params?: unknown }[] = []
      ;(window as unknown as { SYNO?: unknown }).SYNO = { SDS: { AppLaunch: (id: string, params?: unknown) => calls.push({ id, params }) } }
      openInExplore({ path: LEVEL })
      const targets: string[] = []
      const wrapper = await mountExplore('normal', backend => mixedLevel(backend, targets), stubs)
      await tile(wrapper, 'daemon.log_0')!.trigger('click')
      await flushPromises()
      expect(calls).toEqual([{ id: 'SYNO.SDS.App.FileStation3.Instance', params: { openfile: '/share62/logs/daemon.log_0' } }])
      expect(document.body.querySelector('[role="dialog"]')).toBeNull()
      delete (window as unknown as { SYNO?: unknown }).SYNO
      expect(targets.at(-1)).toBe(LEVEL)
      await tile(wrapper, '3 more folders')!.trigger('click')
      await tile(wrapper, 'nginx')!.trigger('click')
      await flushPromises()
      expect(targets.at(-1)).toBe(`${LEVEL}/nginx`)
      wrapper.unmount()
    })
  })

  describe('opening items in File Station', () => {
    const LEVEL = '/volume1/share62/logs'
    const FILE_STATION = 'SYNO.SDS.App.FileStation3.Instance'
    // A level with a file and a folder in a share beside DSM's own @docker folder.
    function level(backend: Backend): Backend {
      return async <M extends Method>(method: M, params: ParamsOf<M>): Promise<ResultOf<M>> => {
        if (method !== 'top.list') return backend(method, params)
        const answer = structuredClone(await backend(method, { ...params, target: '/volume1/share62' } as ParamsOf<M>)) as ResultOf<'top.list'>
        const template = answer.results[0].entries.find(entry => entry.kind === 'file')!
        const row = (path: string, kind: 'dir' | 'file', physical: number, blockers: ExploreEntry['blockers'] = []) => ({
          ...structuredClone(template), path, path_hex: undefined, kind, service: null, flags: [], held_by: [], within: undefined, blockers, nlink: 1,
          cleanup_eligible: true, metrics: { ...template.metrics, physical, reclaimable: physical, logical: physical, unknown: 0, pinned_unreferenced: 0 },
        })
        answer.results[0].target = { ...answer.results[0].target, path: LEVEL, path_hex: undefined }
        answer.results[0].entries = [
          row(`${LEVEL}/daemon.log`, 'file', 5 * 1024 * 1024),
          row(`${LEVEL}/nginx`, 'dir', 4 * 1024 * 1024),
          row('/volume1/@docker', 'dir', 3 * 1024 * 1024),
          row(`${LEVEL}/held.log`, 'file', 2 * 1024 * 1024, ['snapshot', 'live_reflink']),
          // A top-level folder under the volume that DSM does not list as a shared folder.
          row('/volume1/scratch/rootfs-1/volume-state.tar', 'file', 1024 * 1024),
          row('/volume1/scratch/rootfs-1/held-state.tar', 'file', 512 * 1024, ['snapshot']),
        ]
        return answer as ResultOf<M>
      }
    }

    function stubFileStation() {
      const calls: { id: string; params?: unknown }[] = []
      ;(window as unknown as { SYNO?: unknown }).SYNO = { SDS: { AppLaunch: (id: string, params?: unknown) => calls.push({ id, params }) } }
      return calls
    }

    function action(wrapper: Awaited<ReturnType<typeof mountExplore>>, path: string) {
      const row = wrapper.findAll('tbody tr').find(candidate => candidate.find(`button[aria-label="${path}"]`).exists())
      return row!.find('[data-testid="explore-file-station"] button')
    }

    afterEach(() => {
      delete (window as unknown as { SYNO?: unknown }).SYNO
    })

    it('opens a file selected in its folder and a folder as itself, without opening the row', async () => {
      const calls = stubFileStation()
      openInExplore({ path: LEVEL })
      const wrapper = await mountExplore('normal', level)
      await action(wrapper, `${LEVEL}/daemon.log`).trigger('click')
      await action(wrapper, `${LEVEL}/nginx`).trigger('click')
      await flushPromises()
      expect(calls).toEqual([
        { id: FILE_STATION, params: { openfile: '/share62/logs/daemon.log' } },
        { id: FILE_STATION, params: { opendir: '/share62/logs/nginx' } },
      ])
      expect(document.body.querySelector('[role="dialog"]')).toBeNull()
      expect(wrapper.findAll(CRUMBS).at(-1)?.text()).toBe('logs')
      wrapper.unmount()
    })

    it('disables the action for a DSM system folder and says why', async () => {
      const calls = stubFileStation()
      openInExplore({ path: LEVEL })
      const wrapper = await mountExplore('normal', level)
      const docker = action(wrapper, '/volume1/@docker')
      expect(docker.attributes('disabled')).toBeDefined()
      await docker.trigger('click')
      expect(calls).toEqual([])
      const tooltips = wrapper.findAllComponents(Tooltip).map(tooltip => String(tooltip.props('content')))
      expect(tooltips).toContain('Not in a shared folder')
      expect(action(wrapper, `${LEVEL}/nginx`).attributes('disabled')).toBeUndefined()
      wrapper.unmount()
    })

    it('opens a plain file in File Station on click, with no detail sheet and no NAS call', async () => {
      const methods: string[] = []
      const calls = stubFileStation()
      openInExplore({ path: LEVEL })
      const wrapper = await mountExplore('normal', backend => level(async (method, params) => {
        methods.push(method)
        return backend(method, params)
      }))
      methods.length = 0
      await wrapper.find(`button[aria-label="${LEVEL}/daemon.log"]`).trigger('click')
      await flushPromises()
      expect(calls).toEqual([{ id: FILE_STATION, params: { openfile: '/share62/logs/daemon.log' } }])
      expect(document.body.querySelector('[role="dialog"]')).toBeNull()
      expect(methods).toEqual([])
      wrapper.unmount()
    })

    it('shows a file with notes in a sheet at once, built from its row, with each note in words', async () => {
      const methods: string[] = []
      const calls = stubFileStation()
      openInExplore({ path: LEVEL })
      const wrapper = await mountExplore('normal', backend => level(async (method, params) => {
        methods.push(method)
        return backend(method, params)
      }))
      methods.length = 0
      await wrapper.find(`button[aria-label="${LEVEL}/held.log"]`).trigger('click')
      await wrapper.vm.$nextTick()
      const dialog = document.body.querySelector('[role="dialog"]')
      expect(dialog?.textContent).toContain('Details: held.log')
      const notes = [...document.body.querySelectorAll('[data-testid="explore-detail-notes"] li')].map(item => item.textContent)
      expect(notes).toEqual([
        'A snapshot still uses some of its data, so deleting it does not free that part',
        'Another file (a copy or clone) shares some of its data',
      ])
      expect(document.body.querySelector('[data-testid="explore-detail-size"]')?.textContent).toBe(`Size ${formatBytes(2 * 1024 * 1024)}, ${formatBytes(2 * 1024 * 1024)} can be freed`)
      expect(dialog?.textContent).not.toContain('Syno Reclaim found')
      // The where-to-manage entry stays reachable when the column is hidden
      expect(document.body.querySelector('[data-testid="explore-detail-manage"]')?.textContent).toBeTruthy()
      expect(methods).toEqual([])
      document.body.querySelector<HTMLButtonElement>('[data-testid="explore-detail-file-station"]')!.click()
      expect(calls).toEqual([{ id: FILE_STATION, params: { openfile: '/share62/logs/held.log' } }])
      wrapper.unmount()
    })

    it('never sends a file outside the shared folders to File Station and says why', async () => {
      const calls = stubFileStation()
      openInExplore({ path: LEVEL })
      const wrapper = await mountExplore('normal', level)
      expect(action(wrapper, '/volume1/scratch/rootfs-1/volume-state.tar').attributes('disabled')).toBeDefined()
      await wrapper.find('button[aria-label="/volume1/scratch/rootfs-1/volume-state.tar"]').trigger('click')
      await flushPromises()
      expect(document.body.querySelector('[role="dialog"]')).toBeNull()
      expect(document.body.textContent).toContain('Not in a shared folder')

      await wrapper.find('button[aria-label="/volume1/scratch/rootfs-1/held-state.tar"]').trigger('click')
      await flushPromises()
      expect(document.body.querySelector('[data-testid="explore-detail-file-station"]')).toBeNull()
      expect(document.body.querySelector('[data-testid="explore-detail-file-station-reason"]')?.textContent).toBe('Not in a shared folder')
      expect(calls).toEqual([])
      wrapper.unmount()
    })

    it('toasts when File Station could not be launched', async () => {
      openInExplore({ path: LEVEL })
      const wrapper = await mountExplore('normal', level)
      await wrapper.find(`button[aria-label="${LEVEL}/daemon.log"]`).trigger('click')
      await flushPromises()
      expect(document.body.textContent).toContain('Could not open File Station')
      wrapper.unmount()
    })
  })

  it('opens the Packages page for a clicked service row instead of a detail sheet', async () => {
    const wrapper = await mountExplore()
    const serviceRow = wrapper.findAll('tbody tr').find(row => row.text().includes('Synology Drive Server'))
    await serviceRow!.find('button[aria-label="/volume1/share03"]').trigger('click')
    expect(nav.page).toBe('packages')
    expect(takePackageRequest()).toMatchObject({ path: '/volume1/share03' })
    wrapper.unmount()
  })

  it('opens at the path another page asked for', async () => {
    openInExplore({ path: '/volume1/share03' })
    const targets: string[] = []
    const wrapper = await mountExplore('interaction', backend => (method, params) => {
      if (method === 'top.list') targets.push(String((params as { target: unknown }).target && ((params as { target: { path?: string } }).target.path ?? (params as { target: unknown }).target)))
      return backend(method, params)
    })
    expect(targets).toContain('/volume1/share03')
    wrapper.unmount()
  })

  it('shows DSM services and browse-only shares from volumes.list', async () => {
    const wrapper = await mountExplore()

    expect(wrapper.text()).toContain('Synology Drive Server')
    expect(wrapper.text()).toContain('Drive Admin Console')
    expect(wrapper.text()).toContain('Container Manager')
    expect(wrapper.text()).toContain('Surveillance Station recordings')
    const serviceRow = wrapper.findAll('tbody tr').find(row => row.text().includes('Synology Drive Server'))
    const serviceButton = serviceRow?.find('button[aria-label="/volume1/share03"]')
    expect(serviceButton?.find('[data-ui-cell-primary]').text()).toBe('Synology Drive Server')
    expect(serviceButton?.find('[data-ui-cell-secondary]').text()).toBe('DSM service data · share03')
    expect(wrapper.findAll('svg[role="group"] g[role="button"]').some(tile =>
      tile.attributes('aria-label')?.startsWith('Synology Drive Server:'),
    )).toBe(true)
    const serviceReason = serviceRow?.find('[data-ui-grid-select-reason]')
    expect(serviceReason?.attributes('aria-label')).toBe('Managed by Synology Drive Server. Clean it up there')
    await serviceReason!.find('button').trigger('click')
    expect(wrapper.text()).not.toContain('item in the cleanup list')

    const sharePath = '/volume1/share04'
    const shareRow = wrapper.findAll('tbody tr').find(row => row.find(`button[aria-label="${sharePath}"]`).exists())
    expect(shareRow?.text()).toContain('unknown')
    expect(shareRow?.text()).not.toContain('0 B')
    expect(shareRow!.find('[data-ui-grid-select-reason]').attributes('aria-label')).toBe('Size and reclaimable values are unavailable here. Open this share to browse it')
    await shareRow!.find('[data-ui-grid-select-reason] button').trigger('click')
    expect(wrapper.text()).not.toContain('item in the cleanup list')
    const share = shareRow?.find(`button[aria-label="${sharePath}"]`)
    expect(share?.exists()).toBe(true)
    await share!.trigger('click')
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()

    expect(wrapper.find('button[aria-label="/volume1/share04/folder0005"]').exists()).toBe(true)
    const recycle = wrapper.findAll('tbody td:nth-child(6) [data-ui-cell-secondary]').find(line => line.text() === 'Recycle Bin')
    expect(recycle?.element.previousElementSibling?.textContent).toBe('Shared Folder settings')

    const volumeCrumb = wrapper.find('[aria-label="Path"] > ol:not([aria-hidden]) button[title="volume1"]')
    await volumeCrumb.trigger('click')
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    expect(wrapper.text()).toContain('Synology Drive Server')
    wrapper.unmount()
  })

  it('says how many items a folder holds, from the scan\'s entry count, and nothing for a file', async () => {
    const wrapper = await mountExplore()
    await wrapper.find('button[aria-label="/volume1/share04"]').trigger('click')
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    const folder = wrapper.find('button[aria-label="/volume1/share04/folder0002"] [data-ui-cell-secondary]')
    expect(folder.text()).toContain('1 item')
    const filesTab = wrapper.findAll('button').find(button => button.text().trim() === 'By file size')!
    await filesTab.trigger('click')
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    const file = wrapper.find('button[aria-label="/volume1/share04/folder0002/file0001.bin"]')
    expect(file.text()).not.toMatch(/\bitems?\b/)
    wrapper.unmount()
  })

  it('switches top-list views and filters the current path', async () => {
    const wrapper = await mountExplore()
    await wrapper.find('button[aria-label="/volume1/share04"]').trigger('click')
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    const filesTab = wrapper.findAll('button').find(button => button.text().trim() === 'By file size')
    expect(filesTab).toBeDefined()
    await filesTab!.trigger('click')
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    expect(wrapper.find('button[aria-label="/volume1/share04/folder0002/file0001.bin"]').exists()).toBe(true)
    const longPathRow = wrapper.findAll('tbody tr').find(row =>
      row.find('button[aria-label="/volume1/share04/entry0001/entry0002/entry0003/file0002.bin"]').exists(),
    )
    const context = longPathRow?.find('[data-ui-cell-secondary]')
    expect(context?.text()).toBe('entry0001/entry0002/entry0003')
    expect(context?.attributes('title')).toBeUndefined()
    expect(longPathRow?.findAll('td').at(5)?.find('span').text()).toBe('Active Backup for Business')
    const hyperBackupRow = wrapper.findAll('tbody tr').find(row =>
      row.find('button[aria-label="/volume1/share04/entry0006/entry0007/entry0008/entry0009/file0004.bin"]').exists(),
    )
    expect(hyperBackupRow?.findAll('td').at(5)?.find('span').text()).toBe('Hyper Backup')

    const search = wrapper.find('input[placeholder="Search this path"]')
    await search.setValue('file0001.bin')
    expect(wrapper.find('button[aria-label="/volume1/share04/folder0002/file0001.bin"]').exists()).toBe(true)
    expect(wrapper.find('button[aria-label="/volume1/share04/entry0001/entry0002/entry0003/file0002.bin"]').exists()).toBe(false)
    wrapper.unmount()
  })

  // One info button beside the search bar holds the page's notes; none takes a row.
  it('explains the search limit and the shares without sizes from the info button beside search, not a row', async () => {
    const wrapper = await mountExplore()
    const help = 'Searches only the rows loaded so far (first 200)'
    const missing = 'The top-level results have no size values for some shares. Open a share to browse it'
    expect(wrapper.text()).not.toContain(help)
    expect(wrapper.text()).not.toContain(missing)
    expect(wrapper.findAllComponents(Callout)).toHaveLength(0)
    const info = wrapper.get('[data-testid="page-info"]')
    expect(info.attributes('type')).toBe('button')
    // Same line as the search bar: the control sits next to the search input.
    expect(info.element.closest('div')?.contains(wrapper.find('input[placeholder="Search this path"]').element)).toBe(true)
    expect(await openPageInfo(wrapper)).toEqual([help, missing])
    wrapper.unmount()
  })

  // One volume offers no choice; with more, one compact dropdown sits in the
  // page bar before the status, and the page keeps no row of its own for it.
  it('shows the volume dropdown in the page bar only when there is more than one volume', async () => {
    const wrapper = await mountExplore()
    expect(document.querySelector('.volume-picker')).toBeNull()
    const [first] = volumes.document!.volumes
    volumes.document!.volumes.push({ ...first!, mount: { ...first!.mount, path: '/volume2' } })
    await flushPromises()
    const picker = document.querySelector('.volume-picker--bar')
    expect(picker).not.toBeNull()
    expect(wrapper.find('.explore-page .volume-picker').exists()).toBe(false)
    expect(picker!.querySelector('[data-ui-tab-value]')).toBeNull()
    expect(picker!.querySelector('[aria-label="Volume"]')).not.toBeNull()
    wrapper.unmount()
  })

  // The select column is a box wide: the box's accessible name says what it does.
  it('names each row box after what it does to the cleanup list', async () => {
    const wrapper = await mountExplore()
    await openShare04(wrapper)
    const box = rowBox(wrapper, '/volume1/share04/folder0005')!
    expect(box.attributes('aria-label')).toBe('Add folder0005 to the cleanup list')
    await box.trigger('click')
    await flushPromises()
    expect(rowBox(wrapper, '/volume1/share04/folder0005')!.attributes('aria-label')).toBe('Remove folder0005 from the cleanup list')
    wrapper.unmount()
  })

  it('selects every selectable row from the header box, skips disabled rows, and clears on a second click', async () => {
    const wrapper = await mountExplore()
    await openShare04(wrapper)
    const header = () => wrapper.get('[data-ui-grid-select-all] button[role="checkbox"]')
    const enabled = wrapper.findAll('tbody td:first-child button[role="checkbox"]').filter(box => box.attributes('disabled') === undefined)
    const disabled = wrapper.findAll('tbody [data-ui-grid-select-reason]')
    expect(enabled.length).toBeGreaterThan(1)
    await enabled[0].trigger('click')
    await flushPromises()
    expect(header().attributes('aria-checked')).toBe('mixed')

    await header().trigger('click')
    await flushPromises()
    expect(useBasket('/volume1').items.value).toHaveLength(enabled.length)
    expect(header().attributes('aria-checked')).toBe('true')
    expect(wrapper.findAll('tbody [data-ui-grid-select-reason]')).toHaveLength(disabled.length)

    await header().trigger('click')
    await flushPromises()
    expect(useBasket('/volume1').items.value).toHaveLength(0)
    expect(header().attributes('aria-checked')).toBe('false')
    wrapper.unmount()
  })

  it('selects only the rows a search shows', async () => {
    const wrapper = await mountExplore()
    await openShare04(wrapper)
    await wrapper.find('input[type="search"], input').setValue('folder0005')
    await flushPromises()
    await wrapper.get('[data-ui-grid-select-all] button[role="checkbox"]').trigger('click')
    await flushPromises()
    expect(useBasket('/volume1').items.value.map(item => item.path)).toEqual(['/volume1/share04/folder0005'])
    wrapper.unmount()
  })

  it('shows a row inside a selected folder as going with it, and says why it cannot be chosen alone', async () => {
    const wrapper = await mountExplore()
    await openShare04(wrapper)
    await rowBox(wrapper, '/volume1/share04/folder0003')!.trigger('click')
    await flushPromises()
    const filesTab = wrapper.findAll('button').find(button => button.text().trim() === 'By file size')
    await filesTab!.trigger('click')
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    const row = wrapper.findAll('tbody tr').find(candidate => candidate.find('button[aria-label^="/volume1/share04/folder0003/"]').exists())!
    expect(row.find('[data-ui-cell-secondary]').text()).toContain('Goes with folder0003')
    expect(row.get('[data-ui-grid-select-reason]').attributes('aria-label')).toBe('Goes with folder0003, which is in the cleanup list')
    expect(row.get('[data-ui-grid-select-reason] button').attributes('aria-checked')).toBe('true')
    wrapper.unmount()
  })

  it('lays the treemap and the list out in a split whose separator moves', async () => {
    const wrapper = await mountExplore()
    const separator = wrapper.get('[data-ui-split-pane-separator]')
    expect(separator.attributes('role')).toBe('separator')
    expect(separator.attributes('aria-label')).toBe('Resize the treemap and the list')
    expect(separator.attributes('aria-valuenow')).toBe('56')
    expect(wrapper.get('[data-ui-split-pane-start]').find('.explore-map-panel').exists()).toBe(true)
    expect(wrapper.get('[data-ui-split-pane-end]').find('.explore-data-grid').exists()).toBe(true)
    wrapper.unmount()
  })

  it('keeps the share a drag moves to while the drag goes on, and reverts nothing', async () => {
    localStorage.removeItem('synoreclaim.explore.split:horizontal')
    const rect = vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockReturnValue(
      { x: 0, y: 0, left: 0, top: 0, right: 1012, bottom: 600, width: 1012, height: 600, toJSON: () => ({}) } as DOMRect,
    )
    try {
      const wrapper = await mountExplore()
      const separator = wrapper.get('[data-ui-split-pane-separator]')
      const shares: string[] = []
      const observer = new MutationObserver(() => shares.push(separator.attributes('aria-valuenow')!))
      observer.observe(separator.element, { attributes: true, attributeFilter: ['aria-valuenow'] })
      separator.element.dispatchEvent(new MouseEvent('pointerdown', { button: 0, clientX: 572, bubbles: true }))
      for (const clientX of [506, 456, 406]) {
        window.dispatchEvent(new MouseEvent('pointermove', { clientX }))
        await flushPromises()
      }
      observer.disconnect()
      // Only on release does the SplitPane store the share; until then the page keeps the dragged one.
      expect(separator.attributes('aria-valuenow')).toBe('40')
      expect(shares).toEqual(['50', '45', '40'])
      window.dispatchEvent(new MouseEvent('pointerup'))
      await flushPromises()
      expect(separator.attributes('aria-valuenow')).toBe('40')
      expect(localStorage.getItem('synoreclaim.explore.split:horizontal')).toBe('0.4')
      // A double click clears the saved share, and the default follows the width again.
      await separator.trigger('dblclick')
      await flushPromises()
      expect(separator.attributes('aria-valuenow')).toBe('56')
      expect(localStorage.getItem('synoreclaim.explore.split:horizontal')).toBeNull()
      wrapper.unmount()
    } finally {
      rect.mockRestore()
      localStorage.removeItem('synoreclaim.explore.split:horizontal')
    }
  })

  it('moves the split without drawing the list or the treemap again', async () => {
    localStorage.removeItem('synoreclaim.explore.split:horizontal')
    const rect = vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockReturnValue(
      { x: 0, y: 0, left: 0, top: 0, right: 1012, bottom: 600, width: 1012, height: 600, toJSON: () => ({}) } as DOMRect,
    )
    try {
      const wrapper = await mountExplore()
      // Only the pane's share changes; a size the panes then report is what may lay them out again.
      const renders = { grid: 0, map: 0 }
      for (const [key, component] of [['grid', wrapper.findComponent(DataGrid)], ['map', wrapper.findComponent({ name: 'Treemap' })]] as const) {
        const instance = component.vm.$ as unknown as { render: (...args: unknown[]) => unknown }
        const render = instance.render
        instance.render = function (this: unknown, ...args: unknown[]) {
          renders[key]++
          return render.apply(this, args)
        }
      }
      const separator = wrapper.get('[data-ui-split-pane-separator]')
      separator.element.dispatchEvent(new MouseEvent('pointerdown', { button: 0, clientX: 572, bubbles: true }))
      for (const clientX of [540, 506, 456]) {
        window.dispatchEvent(new MouseEvent('pointermove', { clientX }))
        await flushPromises()
      }
      window.dispatchEvent(new MouseEvent('pointerup'))
      await flushPromises()
      expect(separator.attributes('aria-valuenow')).toBe('45')
      expect(renders).toEqual({ grid: 0, map: 0 })
      wrapper.unmount()
    } finally {
      rect.mockRestore()
      localStorage.removeItem('synoreclaim.explore.split:horizontal')
    }
  })

  it('draws the list again without drawing its cells again', async () => {
    const wrapper = await mountExplore()
    // A moved split or a resized window gives the list new widths and draws it
    // again; its rows are the same, so none of their cells draws again.
    const cells = wrapper.findAllComponents({ name: 'ExploreCell' })
    expect(cells.length).toBeGreaterThan(0)
    let renders = 0
    for (const cell of cells) {
      const instance = cell.vm.$ as unknown as { render: (...args: unknown[]) => unknown }
      const render = instance.render
      instance.render = function (this: unknown, ...args: unknown[]) {
        renders++
        return render.apply(this, args)
      }
    }
    wrapper.findComponent(DataGrid).vm.$forceUpdate()
    await flushPromises()
    expect(renders).toBe(0)
    wrapper.unmount()
  })

  it('draws each size as a bar with its reclaimable part', async () => {
    const wrapper = await mountExplore()
    const bars = wrapper.findAll('tbody [data-ui-size-bar]')
    expect(bars.length).toBeGreaterThan(0)
    expect(bars.some(bar => bar.find('[data-ui-size-bar-part]').exists())).toBe(true)
    wrapper.unmount()
  })

  it('lists what is worth a look at this level, with reasons from the scan', async () => {
    const wrapper = await mountExplore()
    await openShare04(wrapper)
    const trigger = wrapper.get('button.explore-worth')
    expect(trigger.text().replace(/\s+/g, '')).toBe('Worthalook2')
    await trigger.trigger('click')
    await flushPromises()
    const items = [...document.body.querySelectorAll('[role="menuitem"]')].map(item => item.textContent?.trim())
    expect(items).toEqual([
      `folder0005: ${formatBytes(1134592)} kept by snapshots, see Snapshots`,
      `folder0004: ${formatBytes(1048576)} kept by snapshots, see Snapshots`,
    ])
    wrapper.unmount()
  })

  it('says the volume has no usage baseline from the info button, not a row above the page', async () => {
    const wrapper = await mountExplore()
    usageChange.unavailable = ['/volume1']
    await flushPromises()
    expect(await openPageInfo(wrapper)).toContain('volume1: Syno Reclaim can tell you about usage changes after the next complete scan')
    usageChange.unavailable = []
    wrapper.unmount()
  })

  it('shows free space now and after the cleanup list in the basket bar', async () => {
    const wrapper = await mountExplore()
    await openShare04(wrapper)
    await rowBox(wrapper, '/volume1/share04/folder0005')!.trigger('click')
    await flushPromises()
    const meter = wrapper.get('[data-ui-storage-meter]')
    expect(meter.text()).toContain('Free now')
    expect(meter.text()).toContain('Free after cleanup')
    wrapper.unmount()
  })

  it('shows what a volume without cleanup could reclaim but offers nothing to select', async () => {
    const wrapper = await mountExplore('ext4')
    const headers = wrapper.findAll('th').map(th => th.text().trim())
    expect(headers).toContain('Reclaimable')
    expect(headers).not.toContain('Can free')
    const checkboxes = wrapper.findAll('tbody td:first-child button[role="checkbox"]')
    expect(checkboxes.length).toBeGreaterThan(0)
    for (const checkbox of checkboxes) expect(checkbox.attributes('disabled')).toBeDefined()
    const reasons = wrapper.findAll('tbody [data-ui-grid-select-reason]')
    expect(reasons).toHaveLength(checkboxes.length)
    for (const reason of reasons) {
      expect(reason.attributes('aria-label')).toBe("Cleanup isn't available on this volume · Syno Reclaim only reads ext4 volumes")
    }
    expect(wrapper.get('[data-ui-grid-select-all] button[role="checkbox"]').attributes('disabled')).toBeDefined()
    wrapper.unmount()
  })

  it.each(['btrfs', 'ext4'])('adds an eligible %s path to the basket with explicit identity', async filesystem => {
    const wrapper = await mountExplore('interaction', backend => async (method, params) => {
      const doc = await backend(method, params)
      if (filesystem === 'ext4' && method === 'top.list') {
        const top = doc as ResultOf<'top.list'>
        for (const result of top.results) {
          result.report.filesystem = 'ext4'
          result.report.capabilities = { scan: true, cleanup: true, snapshots: false, reflinks: false, cleanup_unavailable_reason: null }
          for (const row of result.entries) { row.inode_device = 64769; row.tree = null }
        }
      }
      return doc
    })
    await wrapper.find('button[aria-label="/volume1/share04"]').trigger('click')
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()

    const filesTab = wrapper.findAll('button').find(button => button.text().trim() === 'By file size')
    await filesTab!.trigger('click')
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()

    const row = wrapper.findAll('tbody tr').find(candidate => candidate.find('button[aria-label="/volume1/share04/folder0002/file0001.bin"]').exists())
    expect(row).toBeDefined()
    const checkbox = row!.find('td:first-child button[role="checkbox"]')
    expect(checkbox.exists()).toBe(true)
    await checkbox.trigger('click')
    await flushPromises()

    expect(wrapper.text().replace(/\u00a0/g, ' ')).toContain(
      'Cleanup list: 1 selected, frees at least 0 B (192 KB held by snapshots)',
    )
    expect(wrapper.text()).toContain('0 B')

    expect(useBasket('/volume1').items.value[0]?.expect).toEqual(filesystem === 'ext4'
      ? { filesystem: 'ext4', filesystem_uuid: expect.any(String), tree: null, inode_device: expect.any(Number), objectid: expect.any(Number) }
      : { filesystem_uuid: expect.any(String), tree: expect.any(Number), objectid: expect.any(Number) })

    const review = wrapper.findAll('button').find(button => button.text().trim() === 'Review and clean up')
    expect(review).toBeDefined()
    await review!.trigger('click')
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    expect(document.body.textContent).toContain('Could not start')
    wrapper.unmount()
  })

  it('does not repeat the volume as a breadcrumb at the volume root', async () => {
    const wrapper = await mountExplore()
    expect(wrapper.find('[aria-label="Path"]').exists()).toBe(false)
    await wrapper.find('button[aria-label="/volume1/share04"]').trigger('click')
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    expect(wrapper.findAll(CRUMBS).map(crumb => crumb.text())).toEqual(['volume1', 'share04'])
    wrapper.unmount()
  })

  it('keeps the full held-by text available when the column is narrow', async () => {
    const wrapper = await mountExplore()
    const heldCells = wrapper.findAll('tbody td .explore-held [data-ui-cell-primary]')
    expect(heldCells.length).toBeGreaterThan(0)
    for (const cell of heldCells) {
      const text = cell.text()
      if (text) expect(cell.element.closest('[aria-label]')?.getAttribute('aria-label')).toBe(text)
    }
    wrapper.unmount()
  })

  // At 1440px the DSM window leaves Where to manage about 135px: the
  // place leads, its action goes to a second line, and the whole label, with the
  // service the name column already shows, is the cell's tooltip.
  it('splits Where to manage into the place and its action, with the whole label as tooltip', async () => {
    const wrapper = await mountExplore()
    const row = wrapper.findAll('tbody tr').find(candidate => candidate.find('td:nth-child(2) [data-ui-cell-primary]').text() === 'Container Manager')
    const where = row!.find('td:nth-child(6)')
    expect(where.find('[data-ui-cell-primary]').text()).toBe('Container Manager')
    expect(where.find('[data-ui-cell-secondary]').text()).toBe('remove unused images and containers')
    expect(where.findComponent(Tooltip).props('content'))
      .toBe('Managed by Container Manager · Container Manager · remove unused images and containers')
    wrapper.unmount()
  })

  it.each([
    ['no_report', 'Not scanned yet'],
    ['busy', 'Another scan is using this volume'],
    ['error', 'Something went wrong'],
  ])('shows the %s state', async (scenario, title) => {
    const wrapper = await mountExplore(scenario)
    expect(wrapper.text()).toContain(title)
    wrapper.unmount()
  })

  // A failed answer has no rows, so an empty map and table under
  // the state would read as a second, contradicting empty state.
  it('shows only the busy state, without an empty map and table', async () => {
    const wrapper = await mountExplore('busy')
    expect(wrapper.find('.explore-layout').exists()).toBe(false)
    wrapper.unmount()
  })

  it('shows only the stopped-backend state, without an empty map and table', async () => {
    const wrapper = await mountExplore('interaction', backend => async (method, params) => {
      if (method === 'top.list') throw new ApiError('backend_unavailable')
      return backend(method, params)
    })
    expect(wrapper.find('.explore-layout').exists()).toBe(false)
    wrapper.unmount()
  })

  it('keeps the shares to browse under a failed answer', async () => {
    const wrapper = await mountExplore('error')
    expect(wrapper.text()).toContain('Something went wrong')
    expect(wrapper.find('.explore-layout').exists()).toBe(true)
    wrapper.unmount()
  })

  it('keeps a stale report in the header chip without a separate info row', async () => {
    const wrapper = await mountExplore('stale')
    // The toolbar freshness chip stays; the redundant scan-age info row is gone.
    expect(pageStatusText()).toContain('No changes seen')
    expect(wrapper.text()).not.toContain('It will be re-checked before cleanup')
    expect(wrapper.findAllComponents(Callout).some(callout => String(callout.props('title')).includes('Last scanned'))).toBe(false)
    wrapper.unmount()
  })

  it('offers the failure reason and a retry when the answer fails', async () => {
    let topCalls = 0
    const wrapper = await mountExplore('error', backend => async (method, params) => {
      if (method === 'top.list') topCalls += 1
      return backend(method, params)
    })
    expect(wrapper.text()).toContain('Something went wrong')
    expect(wrapper.findAll('.app-shell ul li').length).toBeGreaterThan(0)
    const retry = wrapper.findAll('button').find(button => button.text().trim() === 'Retry')
    expect(retry, wrapper.text()).toBeDefined()
    const before = topCalls
    clearAnswers()
    await retry!.trigger('click')
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    expect(topCalls).toBeGreaterThan(before)
    wrapper.unmount()
  })

  it('keeps stale timing separate from compact partial status', async () => {
    const wrapper = await mountExplore('partial_stale', fewRows)

    expect(wrapper.findAllComponents(Callout).some(callout => callout.props('tone') === 'warning')).toBe(false)
    expect(String(wrapper.getComponent(ScanBadge).props('details')).includes('Some data could not be read')).toBe(true)
    // Stale age lives in the header freshness chip; partial stays a quiet tooltip.
    expect(pageStatusText()).toContain('Not fully read')
    wrapper.unmount()
  })

  it('reassures when only system areas changed during the scan', async () => {
    const wrapper = await mountExplore('changed_system', fewRows)
    expect(wrapper.text()).toContain('The filesystem changed during this scan')
    expect(wrapper.find('.system-areas-note').text()).toBe(
      "Changes were only in system areas (DSM's own writes); cleanup in shared folders is not affected",
    )
    wrapper.unmount()
  })

  it('has no system-areas note when a shared folder changed', async () => {
    const wrapper = await mountExplore('changed_share', fewRows)
    expect(wrapper.find('.system-areas-note').exists()).toBe(false)
    wrapper.unmount()
  })

  describe('table width', () => {
    type Observed = { element: Element; callback: ResizeObserverCallback }
    let observed: Observed[] = []

    class FakeResizeObserver {
      constructor(private callback: ResizeObserverCallback) {}
      observe(element: Element) { observed.push({ element, callback: this.callback }) }
      disconnect() {}
    }

    function resize(selector: string, width: number, height = 400) {
      for (const { callback } of observed.filter(item => item.element.matches(selector))) {
        callback([{ contentRect: { width, height } } as ResizeObserverEntry], {} as ResizeObserver)
      }
    }

    const headers = (wrapper: Awaited<ReturnType<typeof mountExplore>>) => wrapper.findAll('thead th').map(th => th.text())

    beforeEach(() => {
      observed = []
      vi.stubGlobal('ResizeObserver', FakeResizeObserver)
    })
    afterEach(() => vi.unstubAllGlobals())

    it('folds Held by and Where to manage under the name in a narrow list and brings them back when it widens', async () => {
      const wrapper = await mountExplore('share_scan')
      expect(headers(wrapper)).toContain('Where to manage')
      // A row something holds: its Held by text moves under its name.
      const heldColumn = headers(wrapper).indexOf('Held by')
      const heldRow = wrapper.findAll('tbody tr[data-row-key]').find(row => row.findAll('td')[heldColumn]?.text().includes('snapshot'))!
      const key = heldRow.attributes('data-row-key')
      const heldText = heldRow.findAll('td')[heldColumn].text()
      const context = () => wrapper.find(`tbody tr[data-row-key="${key}"] [data-ui-cell-secondary]`).text()
      expect(context()).not.toContain(heldText)

      resize('.explore-data-grid', 525)
      await flushPromises()
      expect(headers(wrapper).filter(Boolean)).toEqual(['Name', 'Size', 'Can free'])
      expect(context()).toContain(heldText)
      // The compact list keeps each line to one, its full text in the tooltip.
      const lines = wrapper.findAll('tbody [data-ui-cell-primary], tbody [data-ui-cell-secondary]')
      expect(lines.length).toBeGreaterThan(0)
      for (const line of lines) expect(line.element.closest('[data-layout]')?.hasAttribute('data-wrap')).toBe(false)

      resize('.explore-data-grid', 760)
      await flushPromises()
      expect(headers(wrapper)).toContain('Held by')
      expect(headers(wrapper)).toContain('Where to manage')
      expect(context()).not.toContain(heldText)
      wrapper.unmount()
    })

    it('lets the name column take the width the fixed columns leave', async () => {
      const wrapper = await mountExplore()
      const name = wrapper.findAll('thead th').find(th => th.text().startsWith('Name'))
      expect(name?.attributes('style') ?? '').not.toContain('width')
      wrapper.unmount()
    })

    // A label such as "Add to review list" fits at the width of a full-page
    // Portal (a 640px table); a wider table gives both text columns more room.
    it('shares the room the fixed columns leave between the name and Where to manage', async () => {
      const wrapper = await mountExplore()
      const width = (key: string) => {
        const columns = wrapper.findComponent(DataGrid).props('columns') as DataGridColumn[]
        return Number.parseFloat(String(columns.find(column => column.key === key)?.width))
      }
      resize('.explore-data-grid', 700)
      await flushPromises()
      expect(width('location')).toBeGreaterThanOrEqual(120)
      expect(width('name')).toBeGreaterThanOrEqual(170)
      const narrow = { where: width('location'), name: width('name') }
      resize('.explore-data-grid', 1000)
      await flushPromises()
      expect(width('location')).toBeGreaterThan(narrow.where)
      expect(width('name')).toBeGreaterThan(narrow.name)
      wrapper.unmount()
    })

    it('moves the treemap above the table when the content area is narrow', async () => {
      const wrapper = await mountExplore()
      const layout = () => wrapper.find('.explore-layout')
      const orientation = () => wrapper.get('[data-ui-split-pane]').attributes('data-orientation')
      expect(layout().classes()).not.toContain('explore-layout--stacked')
      expect(orientation()).toBe('horizontal')
      resize('.explore-layout', 640)
      await flushPromises()
      expect(layout().classes()).toContain('explore-layout--stacked')
      expect(orientation()).toBe('vertical')
      resize('.explore-layout', 980)
      await flushPromises()
      expect(layout().classes()).not.toContain('explore-layout--stacked')
      wrapper.unmount()
    })

    it('keeps the colour key behind an icon when stacked or when the map is narrow', async () => {
      const wrapper = await mountExplore()
      const inline = () => wrapper.find('.explore-map-panel > ul.explore-legend').exists()
      const tip = () => wrapper.find('[data-testid="explore-legend-tip"]').exists()
      expect(inline()).toBe(true)
      expect(tip()).toBe(false)
      resize('.explore-layout', 640)
      await flushPromises()
      expect(inline()).toBe(false)
      expect(tip()).toBe(true)
      resize('.explore-layout', 980)
      resize('.explore-map-panel', 400)
      await flushPromises()
      expect(inline()).toBe(false)
      expect(tip()).toBe(true)
      resize('.explore-map-panel', 600)
      await flushPromises()
      expect(inline()).toBe(true)
      wrapper.unmount()
    })

    it('keeps the cleanup bar to one short row in a short window', async () => {
      // Keep the exact estimate pending: this layout case exercises the lower bound.
      const estimate = deferred<ResultOf<'simulate.get'>>()
      const wrapper = await mountExplore('interaction', backend => async (method, params) => {
        if (method === 'simulate.get') return estimate.promise as never
        return backend(method, params)
      })
      await openShare04(wrapper)
      await rowBox(wrapper, '/volume1/share04/folder0005')!.trigger('click')
      await flushPromises()
      const bar = () => wrapper.get('.explore-basket-bar')
      const clear = () => bar().findAll('button').find(button => button.attributes('aria-label') === 'Clear list' || button.text() === 'Clear list')
      resize('.explore-page', 1400, 900)
      await flushPromises()
      expect(bar().text()).toContain('Cleanup list: 1 selected')
      expect(clear()?.text()).toBe('Clear list')

      resize('.explore-page', 1000, 640)
      await flushPromises()
      expect(bar().text()).toContain('1 selected, frees at least')
      expect(bar().text()).not.toContain('Cleanup list:')
      expect(clear()?.text()).toBe('')
      expect(clear()?.attributes('aria-label')).toBe('Clear list')
      expect(bar().find('[title^="Cleanup list: 1 selected"]').exists()).toBe(true)
      wrapper.unmount()
    })
  })
})
