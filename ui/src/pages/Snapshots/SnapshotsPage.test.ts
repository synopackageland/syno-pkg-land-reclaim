import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'
import { ApiError, setBackend } from '@/api'
import { Select } from '@synopackageland/ui'
import type { Backend, Method } from '@/api/methods'
import { allowAuthorizationForTests } from '@/authorizationFixture'
import { nav } from '@/stores/nav'
import { fixtureBackend } from '@/api/fixtures'
import { loadVolumes, volumes } from '@/stores/volumes'
import { jobs } from '@/stores/jobs'
import many from '../../../fixtures/snapshots.list/volume1-many.json'
import manySimulated from '../../../fixtures/snapshots.list/volume1-many-simulated.json'
import manySimulatedExact from '../../../fixtures/snapshots.list/volume1-many-simulated-exact.json'
import noSnapshots from '../../../fixtures/snapshots.list/no-snapshots.json'
import { showPageActionsInPlace } from '@/shell/pageActionsInPlace'
import ScanBadge from '@/components/ScanBadge.vue'
import { openPageStatus, pageStatusText } from '@/components/pageInfoTesting'
import { setDsmInventorySource, createDsmInventorySource, dsmInventorySource } from '@/dsm/snapshotInventory'
import { fixtureInventorySource } from '@/dsm/snapshotInventoryFixture'
import SnapshotsPage from './SnapshotsPage.vue'

showPageActionsInPlace()

/** Follow the page's lazy reads until it leaves its loading state. */
async function settle(wrapper: VueWrapper) {
  for (let i = 0; i < 50 && wrapper.find('[role="status"][aria-label="Loading"]').exists(); i++) {
    await flushPromises()
    await vi.dynamicImportSettled()
  }
  await flushPromises()
}

async function loadVolumesWith(scenario = 'interaction') {
  setBackend(fixtureBackend({ scenario }))
  volumes.document = null
  volumes.selected = null
  await loadVolumes()
}

async function mountPage(scenario = 'interaction') {
  await loadVolumesWith(scenario)
  const wrapper = mount(SnapshotsPage, { attachTo: document.body })
  await settle(wrapper)
  return wrapper
}

function rowOf(wrapper: VueWrapper, key: string) {
  return wrapper.find(`tr[data-row-key="${key}"]`)
}
function rows(wrapper: VueWrapper) {
  return wrapper.findAll('.snapshot-table tr[data-row-key]')
}
async function tick(wrapper: VueWrapper, key: string, shiftKey = false) {
  await rowOf(wrapper, key).find('[role="checkbox"]').trigger('click', { shiftKey })
}

/**
 * Select bars and let the page's 400 ms debounce run on fake timers, then wait
 * for the lazily imported answer. Real sleeps raced the debounce on slow CI.
 */
async function selectSnapshots(wrapper: VueWrapper, ...keys: string[]) {
  vi.useFakeTimers()
  try {
    for (const key of keys) await tick(wrapper, key)
    await vi.advanceTimersByTimeAsync(400)
  } finally {
    vi.useRealTimers()
  }
  await vi.dynamicImportSettled()
  await flushPromises()
}


describe('SnapshotsPage', () => {
  it('includes a newer dedicated share snapshot report beside an older volume report', async () => {
    const base = fixtureBackend({ scenario: 'many_snapshots' })
    await loadVolumesWith('many_snapshots')
    const share = structuredClone(many.results[0]!.shares[0]!)
    share.share.path = 'fresh-snapshot-share'
    setBackend((async (method, params) => {
      if (method === 'history.list') {
        const history = await base('history.list', {})
        return { ...history, scans: [{ ...history.scans[0]!, volume: false, report: '/private/new.srrep', outcome: 'completed', finished: '2030-01-01T00:00:00Z', targets: [{ path: '/volume1/fresh-snapshot-share' }] }] }
      }
      if (method === 'snapshots.list' && JSON.stringify(params).includes('/volume1/fresh-snapshot-share')) {
        return { ...many, results: [{ ...many.results[0]!, report: { ...many.results[0]!.report, scan_finished: '2030-01-01T00:00:00Z' }, shares: [share] }] }
      }
      return base(method, params)
    }) as Backend)
    const wrapper = mount(SnapshotsPage, { attachTo: document.body })
    try {
      await settle(wrapper)
      await vi.dynamicImportSettled()
      await flushPromises()
      const picker = wrapper.findComponent(Select)
      expect(JSON.stringify(picker?.props('options'))).toContain('fresh-snapshot-share')
    } finally { wrapper.unmount() }
  })
  afterEach(() => { jobs.list = [] })
  it('lists every unanalysed snapshot under its shared folder with its DSM name and date', async () => {
    const wrapper = await mountPage('many_snapshots')
    const groups = wrapper.find('[data-testid="unanalysed-snapshots"]')
    expect(groups.exists()).toBe(true)
    const unread = many.results[0]!.shares.flatMap(share => share.snapshots.filter(snapshot => !snapshot.materialized))
    expect(groups.findAll('li')).toHaveLength(unread.length)
    for (const snapshot of unread) expect(groups.text()).toContain(snapshot.name.path)
    expect(groups.text()).toContain('not analysed')
    expect(groups.find('button').text()).toContain('Analyse all snapshots')
    wrapper.unmount()
  })

  it('queues analysis of the saved report without creating a new namespace scan', async () => {
    allowAuthorizationForTests()
    jobs.list = []
    await loadVolumesWith('many_snapshots')
    const recorded = fixtureBackend({ scenario: 'many_snapshots' })
    const calls: { method: Method; params: unknown }[] = []
    setBackend((async (method: Method, params: unknown) => {
      calls.push({ method, params })
      if (method === 'scan.start') return { outcome: 'started', job_id: 'extra-snapshots' }
      return recorded(method, params as never)
    }) as Backend)
    const wrapper = mount(SnapshotsPage, { attachTo: document.body })
    await settle(wrapper)
    await wrapper.find('[data-testid="unanalysed-snapshots"] button').trigger('click')
    await flushPromises()
    expect(calls.find(call => call.method === 'scan.start')?.params).toEqual({
      targets: [volumes.document!.volumes[0]!.mount], mode: 'gentle', snapshots: 'all', snapshot_report: many.results[0]!.report.job_id,
    })
    expect(nav.followedJobId).toBe('extra-snapshots')
    wrapper.unmount()
  })

  it.each([false, true])('offers a saved-report update after every snapshot was analysed (busy: %s)', async busy => {
    allowAuthorizationForTests()
    jobs.list = []
    await loadVolumesWith('many_snapshots')
    const recorded = fixtureBackend({ scenario: 'many_snapshots' })
    const complete = structuredClone(many)
    for (const share of complete.results[0]!.shares) {
      for (const snapshot of share.snapshots) snapshot.materialized = true
    }
    const calls: unknown[] = []
    setBackend((async (method: Method, params: unknown) => {
      if (method === 'snapshots.list') return complete
      if (method === 'scan.start') { calls.push(params); return { outcome: 'started', job_id: 'snapshot-update' } }
      return recorded(method, params as never)
    }) as Backend)
    const wrapper = mount(SnapshotsPage, { attachTo: document.body })
    await settle(wrapper)
    expect(wrapper.find('[data-testid="unanalysed-snapshots"]').exists()).toBe(false)
    await vi.dynamicImportSettled()
    await flushPromises()
    jobs.list = busy ? [{ kind: 'scan', state: 'running', filesystems: [], targets: [], job_id: 'already-scanning' } as unknown as typeof jobs.list[number]] : []
    await flushPromises()
    const refresh = wrapper.find('[data-testid="snapshot-refresh"]')
    expect(refresh.exists()).toBe(true)
    expect(refresh.attributes('disabled') !== undefined).toBe(busy)
    if (!busy) {
      await refresh.trigger('click')
      await flushPromises()
      expect(calls).toEqual([{
        targets: [volumes.document!.volumes[0]!.mount], mode: 'gentle', snapshots: 'all', snapshot_report: complete.results[0]!.report.job_id,
      }])
      expect(nav.followedJobId).toBe('snapshot-update')
    }
    wrapper.unmount()
    jobs.list = []
  })

  it('walks one flow from top to bottom: shared folder, snapshots, what it frees, where to delete', async () => {
    const wrapper = await mountPage('many_snapshots')
    const steps = wrapper.findAll('ol.steps > li')
    expect(steps.map(step => step.find('h2').text())).toEqual([
      '1Choose a shared folder, LUN or VM',
      '2Select the snapshots to delete',
      '3Space you can free',
      '4Delete them in DSM',
    ])
    expect(steps[0]!.find('select, button, [role="combobox"]').exists()).toBe(true)
    expect(steps[1]!.findAll('tr[data-row-key]').length).toBeGreaterThan(0)
    expect(steps[2]!.text()).toContain('Select snapshots to see what deleting them frees')
    expect(steps[3]!.find('button').text()).toContain('Delete in')
    expect(wrapper.findAll('.muted')).toHaveLength(0)
    wrapper.unmount()
  })

  it('names a shared folder target by its type alone, with no trailing separator', async () => {
    const wrapper = await mountPage('many_snapshots')
    expect(wrapper.find('[data-testid="target-kind"]').text()).toBe('Shared folder')
    wrapper.unmount()
  })

  it('draws each numbered step as a UI surface', async () => {
    const wrapper = await mountPage('many_snapshots')
    expect(wrapper.findAll('.steps > li > [data-ui-surface][data-variant="default"]')).toHaveLength(4)
    wrapper.unmount()
  })

  it('keeps the reasons an answer is not exact in notes, the bound in view', async () => {
    const wrapper = await mountPage('many_snapshots')
    await selectSnapshots(wrapper, '2566', '2569')
    const result = wrapper.find('.result-card')
    expect(result.text()).toContain('up to 15.4 GB')
    expect(result.find('[data-testid="scan-notes"]').text()).toContain('Some owners are unknown')
    wrapper.unmount()
  })

  it('still lists the snapshots when the selected share holds no snapshot-exclusive space', async () => {
    const wrapper = await mountPage()
    expect(wrapper.find('.held').text()).toContain('0 B')
    expect(rows(wrapper).length).toBeGreaterThan(0)
    wrapper.unmount()
  })

  it('keeps the real snapshot report partial and defaults to the share with the most held space', async () => {
    const wrapper = await mountPage('normal')
    expect(wrapper.text()).toContain('Some data could not be read')
    expect(wrapper.text()).toContain('This scan did not read 6 snapshots')
    expect(wrapper.find('.held').text()).toContain('20 KB')
    expect(rows(wrapper)).toHaveLength(2)
    wrapper.unmount()
  })

  // The bar chart's axis ran labels together ("9/30 上午9/35 上午11:1110/22"): each row has its full time.
  it('shows each snapshot with its full date and time, newest first', async () => {
    const wrapper = await mountPage('many_snapshots')
    const times = rows(wrapper).map(row => row.find('[data-snapshot-key] [data-ui-cell-primary]').text())
    expect(times).toHaveLength(6)
    expect(new Set(times).size).toBe(6)
    expect(times.every(time => /2026/.test(time) && /\d{1,2}:\d{2}/.test(time))).toBe(true)
    const keys = rows(wrapper).map(row => row.attributes('data-row-key'))
    expect(keys[0]).toBe('2575')
    wrapper.unmount()
  })

  it('shows the held total, one row per snapshot and the files held by snapshots', async () => {
    const wrapper = await mountPage('many_snapshots')
    expect(wrapper.find('.held [data-ui-statistic-value]').text()).toBe('48.4 GB')
    expect(rows(wrapper)).toHaveLength(6)
    const file = wrapper.findAll('tbody tr').find(row => row.text().includes('raw_2021/wedding_4k'))!
    const [path, , size] = file.findAll('[data-ui-cell-primary]')
    expect(path.element.closest('[data-ui-tooltip-text]')?.getAttribute('data-ui-tooltip-text')).toContain('raw_2021/wedding_4k')
    expect(size.text()).toBe('19.6 GB')
    expect(rowOf(wrapper, '2575').find('[data-column="bytes"]').text()).toBe('Not analysed')
    wrapper.unmount()
  })

  it('computes the freed bytes of the selected snapshots, with bounds and exactness', async () => {
    const wrapper = await mountPage('many_snapshots')
    await selectSnapshots(wrapper, '2566', '2569')
    expect(wrapper.text()).toContain('Delete 2 snapshots')
    expect(wrapper.text()).toContain('15.1 GB')
    expect(wrapper.text()).toContain('up to 15.4 GB')
    expect(wrapper.text()).toContain('Some owners are unknown')
    wrapper.unmount()
  })

  it('says so when the answer is exact', async () => {
    const wrapper = await mountPage('many_snapshots')
    await selectSnapshots(wrapper, '2561')
    expect(wrapper.text()).toContain('16.8 GB')
    expect(wrapper.text()).toContain('Exact: every selected snapshot was read')
    expect(wrapper.find('.result-card [data-ui-statistic-note]').text()).toBe('frees')
    expect(wrapper.find('.result-range').text()).toBe('5/3')
    wrapper.unmount()
  })

  // On a test NAS, the page said "This report has no snapshot data" while Explore
  // showed 494.5 GB of the same report held by snapshots.
  it('shows what the report already knows snapshots hold, and what a scan with snapshots adds', async () => {
    const wrapper = await mountPage('abb_share')
    expect(wrapper.text()).not.toContain('This report has no snapshot data')
    const known = wrapper.find('[data-testid="snapshot-known"]')
    expect(known.text()).toContain('Held by snapshots')
    const row = known.findAll('tbody tr').find(row => row.text().includes('Active Backup for Business (backup data)'))
    expect(row?.text()).toContain('abb-backup')
    expect(row?.text()).toContain('460.5 GB')
    const detail = wrapper.find('[data-testid="snapshot-detail-needed"]')
    expect(detail.text()).toContain('using the saved scan')
    const scan = detail.findAll('button').find(button => button.text() === 'Scan with snapshots')
    expect(scan).toBeDefined()
    wrapper.unmount()
  })

  it('offers analysis from a completed volume scan even when it has no per-snapshot census', async () => {
    const wrapper = await mountPage('abb_share')
    const detail = wrapper.find('[data-testid="snapshot-detail-needed"]')
    expect(detail.findAll('button').some(button => button.text() === 'Analyse all snapshots')).toBe(true)
    expect(detail.text()).toContain('without walking live folders again')
    wrapper.unmount()
  })

  // On a test NAS, every snapshot held 0 B alone, so the page showed 0 B everywhere while
  // Overview said old snapshots hold up to 46.4 GB. The space has an unknown owner or is shared.
  describe('when no snapshot holds space by itself', () => {
    it('opens the folder whose snapshots may hold the most, with that bound', async () => {
      const wrapper = await mountPage('snapshot_unknown')
      expect(wrapper.find('.share').text()).toContain('Backup')
      expect(wrapper.find('.held [data-ui-statistic-value]').text()).toBe('Up to 46.4 GB')
      wrapper.unmount()
    })

    it('keeps the snapshots selectable and says why each one alone frees nothing', async () => {
      const wrapper = await mountPage('snapshot_unknown')
      expect(rowOf(wrapper, '2246').find('[data-column="bytes"]').text()).toBe('shared')
      expect(rowOf(wrapper, '2246').find('[role="checkbox"]').attributes('disabled')).toBeUndefined()
      expect(wrapper.find('[data-testid="snapshot-shared"]').text()).toContain('Shared space is freed only when every snapshot that shares it is deleted')
      wrapper.unmount()
    })

    it('answers a whole range with what deleting it could free, not 0 B', async () => {
      const wrapper = await mountPage('snapshot_unknown')
      vi.useFakeTimers()
      try {
        await wrapper.findAll('button').find(button => button.text() === 'Select all')!.trigger('click')
        await vi.advanceTimersByTimeAsync(400)
      } finally {
        vi.useRealTimers()
      }
      await vi.dynamicImportSettled()
      await flushPromises()
      const result = wrapper.find('.result-card')
      expect(result.find('[data-ui-statistic-value]').text()).toBe('46.4 GB')
      expect(result.find('[data-ui-statistic-note]').text()).toBe('frees up to')
      expect(result.text()).toContain('The scan could not tell how much of it only these snapshots hold')
      expect(result.text()).not.toContain('0 B')
      wrapper.unmount()
    })

    it('lists no 0 B files and says how many files snapshots keep instead', async () => {
      const wrapper = await mountPage('snapshot_unknown')
      const files = wrapper.find('.files-card')
      expect(files.findAll('tbody tr')).toHaveLength(0)
      expect(files.text()).toContain('2507 files are kept only in snapshots, but the scan could not tell how much space they hold')
      expect(files.text()).not.toContain('0 B')
      wrapper.unmount()
    })

    it('says what the scan cannot tell, the most snapshots hold, and what holds space by package', async () => {
      const wrapper = await mountPage('snapshot_unknown')
      const callout = wrapper.find('[data-testid="snapshot-unknown"]')
      expect(callout.text()).toContain('This scan cannot tell exactly what each snapshot holds')
      expect(callout.text()).toContain('46.4 GB')
      expect(callout.text()).toContain('17 snapshots were not read')
      expect(callout.findAll('button').some(button => button.text() === 'Scan with snapshots')).toBe(true)
      expect(wrapper.find('[data-testid="snapshot-known"]').exists()).toBe(true)
      wrapper.unmount()
    })
  })

  it('says the report found nothing snapshots hold, and still offers the scan with snapshots', async () => {
    await loadVolumesWith('no_snapshots')
    const backend = fixtureBackend({ scenario: 'no_snapshots' })
    setBackend(async (method, params) => {
      const answer = await backend(method, params)
      if (method !== 'top.list') return answer
      const doc = structuredClone(answer) as { results: { entries: { held_by: unknown[] }[] }[] }
      for (const entry of doc.results[0]!.entries) entry.held_by = []
      return doc as never
    })
    const wrapper = mount(SnapshotsPage, { attachTo: document.body })
    await settle(wrapper)
    expect(wrapper.text()).toContain('This scan found no space that snapshots also hold')
    expect(wrapper.findAll('button').some(button => button.text() === 'Scan with snapshots')).toBe(true)
    expect(pageStatusText()).toBe('')
    wrapper.unmount()
  })

  it('shows the no-report state when no volume report exists at all', async () => {
    const wrapper = await mountPage('no_report')
    expect(wrapper.text()).toContain('Not scanned yet')
    wrapper.unmount()
  })

  // The header badge carries an old report's age; the page's notes open beside the title,
  // as the tests below read them.
  it('keeps a stale report in the header badge, without a row or a page notes line', async () => {
    await loadVolumesWith()
    const base = fixtureBackend({ scenario: 'interaction' })
    setBackend(async (method, params) => {
      const answer = await base(method, params)
      if (method === 'snapshots.list') (answer as { results: { report: { stale: boolean } }[] }).results[0]!.report.stale = true
      return answer as never
    })
    const wrapper = mount(SnapshotsPage, { attachTo: document.body })
    await settle(wrapper)
    expect(wrapper.getComponent(ScanBadge).props('report').stale).toBe(true)
    expect(wrapper.text()).not.toContain('scan again for current numbers')
    expect(wrapper.find('[data-testid="scan-notes"]').exists()).toBe(false)
    wrapper.unmount()
  })

  it('puts the report time and consistency in the toolbar chip, as Overview and Recycle Bin do', async () => {
    const wrapper = await mountPage()
    expect(pageStatusText()).toContain('No changes seen')
    expect(pageStatusText()).not.toContain('Complete')
    expect(wrapper.findComponent(ScanBadge).props('details')).toContain('No changes seen')
    wrapper.unmount()
  })

  it('reassures when only system areas changed during the scan', async () => {
    const wrapper = await mountPage('changed_system')
    expect(pageStatusText()).toContain('Changed during scan')
    expect((await openPageStatus()).notes).toContain(
      "Changes were only in system areas (DSM's own writes); cleanup in shared folders is not affected",
    )
    wrapper.unmount()
  })

  it('has no system-areas note when a shared folder changed', async () => {
    const wrapper = await mountPage('changed_share')
    expect(wrapper.text()).not.toContain('Changes were only in system areas')
    wrapper.unmount()
  })

  it('shows a muted note, not the banner, for attribution-only unknown above the threshold; the banner stays for a coverage gap', async () => {
    const large = await mountPage('unknown_large')
    expect((await openPageStatus()).notes.join('\n')).toContain('could not be attributed and is not counted as reclaimable')
    expect(large.text()).not.toContain('Some data could not be read')
    large.unmount()
    const gap = await mountPage('coverage_gap')
    expect(gap.text()).toContain('Some data could not be read')
    expect(gap.text()).not.toContain('could not be attributed and is not counted as reclaimable')
    gap.unmount()
  })

  it('shows one muted line, not the banner or partial chip, for a single entry that changed during the scan; eleven keep the banner', async () => {
    const churn = await mountPage('namespace_churn')
    expect((await openPageStatus()).notes).toContain('1 item changed during the scan and was skipped')
    expect(churn.text()).not.toContain('Some data could not be read')
    expect(document.body.textContent).not.toContain('Not fully read')
    churn.unmount()
    const many = await mountPage('namespace_churn_many')
    expect(many.text()).toContain('Some data could not be read')
    expect(many.text()).not.toContain('changed during the scan and was skipped')
    many.unmount()
  })

  it('shows a partial report as one quiet line, not a warning callout', async () => {
    const backend = fixtureBackend()
    await loadVolumesWith()
    setBackend(async (method, params) => {
      if (method === 'snapshots.list') return { ...(many as object), completion: 'partial' } as never
      return backend(method, params)
    })
    const wrapper = mount(SnapshotsPage, { attachTo: document.body })
    await settle(wrapper)
    expect(wrapper.get('[data-testid="partial-hint"]').text()).toContain('Some data could not be read')
    expect(wrapper.find('[data-ui-callout]').exists() && wrapper.html().includes('tone="warning"')).toBe(false)
    wrapper.unmount()
  })

  it('selects a range with shift-click, skipping snapshots already being deleted', async () => {
    const wrapper = await mountPage('many_snapshots')
    const keys = rows(wrapper).map(row => row.attributes('data-row-key')!)
    const selected = () => rows(wrapper).filter(row => row.attributes('aria-selected') === 'true').map(row => row.attributes('data-row-key'))
    // 2572 is already deleted and waits for the cleaner: the range skips it.
    expect(keys.slice(0, 4)).toEqual(['2575', '2572', '2569', '2566'])
    await tick(wrapper, '2575')
    await tick(wrapper, '2566', true)
    expect(selected()).toEqual(['2575', '2569', '2566'])
    // Before Jul 5 (local day): 2564 and 2561.
    const older = wrapper.find('.snapshot-table .older')
    expect(older.exists()).toBe(true)
    await wrapper.findAll('button').find(button => button.text() === 'Clear')!.trigger('click')
    expect(rows(wrapper).some(row => row.attributes('aria-selected') === 'true')).toBe(false)
    wrapper.unmount()
  })

  it('does not start DSM reads after the page left during a report read', async () => {
    await loadVolumesWith('snapshot_targets')
    const recorded = fixtureBackend({ scenario: 'snapshot_targets' })
    let release!: () => void
    setBackend((async (method: Method, params: unknown) => {
      const answer = await recorded(method, params as never)
      if (method === 'snapshots.list') await new Promise<void>(resolve => { release = resolve })
      return answer
    }) as Backend)
    const inventory = vi.fn(async () => null)
    setDsmInventorySource({ ...fixtureInventorySource, inventory })
    const wrapper = mount(SnapshotsPage, { attachTo: document.body })
    try {
      await vi.dynamicImportSettled(); await flushPromises()
      wrapper.unmount()
      release()
      await flushPromises()
      expect(inventory).not.toHaveBeenCalled()
    } finally { wrapper.unmount(); setDsmInventorySource(dsmInventorySource) }
  })

  it('keeps shared-folder metadata loading and cancels it when the target changes or the page leaves', async () => {
    const reads: { name: string; signal?: AbortSignal }[] = []
    setDsmInventorySource({ inventory: async () => null, shareSnapshots: (name, signal) => {
      reads.push({ name, signal })
      return new Promise(resolve => signal?.addEventListener('abort', () => resolve(null), { once: true }))
    } })
    const wrapper = await mountPage('many_snapshots')
    try {
      expect(wrapper.find('[data-testid="dsm-snapshot-loading"]').exists()).toBe(true)
      const first = reads[0]!
      const next = many.results[0]!.shares.find(share => share.share.path !== first.name)!
      wrapper.findComponent(Select).vm.$emit('update:modelValue', next.share.path)
      await flushPromises()
      expect(first.signal?.aborted).toBe(true)
      expect(reads.at(-1)?.name).toBe(next.share.path)
      expect(wrapper.find('[data-testid="dsm-snapshot-loading"]').exists()).toBe(true)
      wrapper.unmount()
      expect(reads.at(-1)?.signal?.aborted).toBe(true)
    } finally { wrapper.unmount(); setDsmInventorySource(dsmInventorySource) }
  })

  describe('snapshot targets that are not shared folders', () => {
    beforeEach(() => setDsmInventorySource(fixtureInventorySource))
    afterEach(() => setDsmInventorySource(dsmInventorySource))

    it('shows loading until a slow DSM inventory settles, then uses its names', async () => {
      let resolve!: (value: Awaited<ReturnType<typeof fixtureInventorySource.inventory>>) => void
      setDsmInventorySource({ ...fixtureInventorySource, inventory: () => new Promise(done => { resolve = done }) })
      const wrapper = await mountPage('snapshot_targets')
      try {
        vi.useFakeTimers()
        await vi.advanceTimersByTimeAsync(60_000)
        expect(wrapper.find('[data-testid="dsm-inventory-loading"]').exists()).toBe(true)
        expect(wrapper.text()).not.toContain('No snapshots')
        resolve(await fixtureInventorySource.inventory())
        await flushPromises()
        expect(wrapper.find('[data-testid="dsm-inventory-loading"]').exists()).toBe(false)
        expect(wrapper.find('.share').text()).toContain('ubuntu-dev')
      } finally { vi.useRealTimers(); wrapper.unmount() }
    })

    it('cancels DSM reads when the page leaves', async () => {
      let signal: AbortSignal | undefined
      setDsmInventorySource({ ...fixtureInventorySource, inventory: next => {
        signal = next
        return new Promise(resolve => next?.addEventListener('abort', () => resolve(null), { once: true }))
      } })
      const wrapper = await mountPage('snapshot_targets')
      wrapper.unmount()
      expect(signal?.aborted).toBe(true)
    })

    it('names LUN and VMM targets from DSM, with type and space, never a bare UUID', async () => {
      const wrapper = await mountPage('snapshot_targets')
      await settle(wrapper)
      const select = wrapper.find('.share')
      expect(select.text()).toContain('ubuntu-dev · 111.8 GB')
      expect(select.text()).not.toMatch(/[0-9a-f]{8}-[0-9a-f]{4}-/)
      expect(wrapper.find('[data-testid="target-kind"]').text()).toBe('VM disk · 7d4e9b20')
      wrapper.unmount()
    })

    it('opens Virtual Machine Manager for a VM disk instead of Snapshot Replication', async () => {
      const launch = vi.fn()
      window.SYNO = { SDS: { AppLaunch: launch } } as never
      const wrapper = await mountPage('snapshot_targets')
      try {
        const action = wrapper.findAll('button').find(button => button.text() === 'Delete in Virtual Machine Manager')
        expect(action).toBeDefined()
        await action!.trigger('click')
        expect(launch).toHaveBeenCalledWith('SYNO.SDS.Virtualization.Application')
      } finally { wrapper.unmount(); delete window.SYNO }
    })

    it.each(['dsm_401', 'dsm_105', 'dsm_error'])('keeps report names and unknown locks after %s', async code => {
      setDsmInventorySource(createDsmInventorySource({ webApi: async () => { throw { code } } }))
      const launch = vi.fn()
      window.SYNO = { SDS: { AppLaunch: launch } } as never
      const wrapper = await mountPage('snapshot_targets')
      try {
        await settle(wrapper)
        expect(wrapper.find('.share').text()).toContain('7d4e9b20')
        expect(rowOf(wrapper, '3101').find('[data-column="lock"]').text()).toBe('–')
        const action = wrapper.findAll('button').find(button => button.text() === 'Delete in SAN Manager')
        await action!.trigger('click')
        expect(launch).toHaveBeenCalledWith('SYNO.SDS.iSCSI.Application')
      } finally { wrapper.unmount(); delete window.SYNO }
    })

    it('keeps a dash when DSM returns a record without a lock flag', async () => {
      setDsmInventorySource({ ...fixtureInventorySource, inventory: async () => {
        const inventory = await fixtureInventorySource.inventory()
        inventory!.lunSnapshots.set('0b1c2d3e-0001-4f00-8a00-000000000001', { locked: null, immutable: false })
        return inventory
      } })
      const wrapper = await mountPage('snapshot_targets')
      try { expect(rowOf(wrapper, '3101').find('[data-column="lock"]').text()).toBe('–') }
      finally { wrapper.unmount() }
    })

    it('shows DSM lock and immutable states per snapshot', async () => {
      const wrapper = await mountPage('snapshot_targets')
      await settle(wrapper)
      expect(rowOf(wrapper, '3101').find('[data-column="lock"] [aria-label="Locked"]').exists()).toBe(true)
      expect(rowOf(wrapper, '3101').text()).toContain('before-upgrade')
      expect(rowOf(wrapper, '3103').find('[data-column="lock"]').text()).toBe('')
      expect(rowOf(wrapper, '3104').find('[data-column="lock"] [aria-label="Locked"]').exists()).toBe(true)
      expect(rowOf(wrapper, '3105').find('[data-column="lock"]').text()).toBe('–')
      wrapper.unmount()
    })
  })

  it('says results are updating while a scan runs on the volume', async () => {
    jobs.list = [{ state: 'running', filesystems: [{ mount: { path: '/volume1' } }] }] as never
    const wrapper = await mountPage()
    expect(wrapper.text()).toContain('Updating results')
    jobs.list = []
    wrapper.unmount()
  })

  it('shows the busy state when the volume is held by another command', async () => {
    await loadVolumesWith()
    setBackend(() => Promise.reject(new ApiError('busy')))
    const wrapper = mount(SnapshotsPage, { attachTo: document.body })
    await settle(wrapper)
    expect(wrapper.text()).toContain('Another scan is using this volume')
    wrapper.unmount()
  })

  it('asks for a new scan when the saved report is from another version', async () => {
    const backend = fixtureBackend()
    await loadVolumesWith()
    const incompatible = {
      ...noSnapshots,
      errors: [{ ...noSnapshots.errors[0], code: 'report_incompatible', message: 'report format 3 is not 4' }],
    }
    setBackend(async (method, params) => (method === 'snapshots.list' ? (incompatible as never) : backend(method, params)))
    const wrapper = mount(SnapshotsPage, { attachTo: document.body })
    await settle(wrapper)
    expect(wrapper.text()).toContain('The saved result is from another version: scan again')
    expect(wrapper.find('button').text()).toContain('Scan')
    expect(wrapper.find('svg.lucide-scan-search').exists()).toBe(true)
    wrapper.unmount()
  })

  it('says so when no shared folder on the volume has snapshots', async () => {
    const backend = fixtureBackend()
    await loadVolumesWith()
    const empty = { ...many, results: [{ ...many.results[0], shares: [] }] }
    setBackend(async (method, params) => (method === 'snapshots.list' ? (empty as never) : backend(method, params)))
    const wrapper = mount(SnapshotsPage, { attachTo: document.body })
    await settle(wrapper)
    expect(wrapper.text()).toContain('No shared folder on this volume has snapshots')
    expect(wrapper.find('svg.lucide-camera').exists()).toBe(true)
    wrapper.unmount()
  })

  it('names the failure and retries it', async () => {
    const backend = fixtureBackend()
    await loadVolumesWith()
    let fail = true
    setBackend(async (method, params) => {
      if (method === 'snapshots.list' && fail) throw new ApiError('timeout')
      return backend(method, params)
    })
    const wrapper = mount(SnapshotsPage, { attachTo: document.body })
    await settle(wrapper)
    expect(wrapper.text()).toContain('The request took too long')
    fail = false
    await wrapper.findAll('button').find(b => b.text() === 'Retry')!.trigger('click')
    await settle(wrapper)
    expect(rows(wrapper)).toHaveLength(2)
    wrapper.unmount()
  })

  it('keeps the answer of the newest selection when an older one returns late', async () => {
    const backend = fixtureBackend({ scenario: 'many_snapshots' })
    await loadVolumesWith('many_snapshots')
    setBackend(async (method, params) => {
      const simulate = (params as { simulate?: string }).simulate
      if (method !== 'snapshots.list' || !simulate) return backend(method, params)
      if (simulate === 'id:2561') {
        await new Promise(resolve => setTimeout(resolve, 700))
        return manySimulatedExact as never
      }
      return manySimulated as never
    })
    const wrapper = mount(SnapshotsPage, { attachTo: document.body })
    await settle(wrapper)
    vi.useFakeTimers()
    try {
      await tick(wrapper, '2561')
      await vi.advanceTimersByTimeAsync(400) // the slow answer for id:2561 is now in flight
      await tick(wrapper, '2561')
      await tick(wrapper, '2566')
      await tick(wrapper, '2569')
      await vi.advanceTimersByTimeAsync(400) // the newest selection answers first
      await vi.advanceTimersByTimeAsync(700) // the older one answers late and must be dropped
    } finally {
      vi.useRealTimers()
    }
    await vi.dynamicImportSettled()
    await flushPromises()
    expect(wrapper.find('.result-card [data-ui-statistic-value]').text()).toBe('15.1 GB')
    wrapper.unmount()
  })
})
