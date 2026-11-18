import { flushPromises, mount } from '@vue/test-utils'
import { watch } from 'vue'
import { Callout, Tooltip } from '@synopackageland/ui'
import { setBackend, ApiError, type Method } from '@/api'
import { code } from '@/i18n'
import { fixtureBackend } from '@/api/fixtures'
import App from '@/App.vue'
import { allowAuthorizationForTests } from '@/authorizationFixture'
import { nav } from '@/stores/nav'
import { readPreferences, setSystemPartitionWarning } from '@/stores/preferences'
import { volumes } from '@/stores/volumes'
import type { VolumesDocument } from '@/types'
import ScanBadge from '@/components/ScanBadge.vue'
import PageStatus from '@/components/PageStatus.vue'
import { pageStatusText } from '@/components/pageInfoTesting'

/** The status details a page shows: the volume cards' tooltips and the page status's details. */
const detailTexts = (wrapper: ReturnType<typeof mount>) => [
  ...wrapper.findAllComponents(Tooltip).map(tooltip => String(tooltip.props('content'))),
  ...wrapper.findAllComponents(ScanBadge).map(badge => String(badge.props('details') ?? '')),
]
import VolumeCard from '@/components/VolumeCard.vue'

// Overview calls use `scenario`; the rest (session, volumes, jobs) stay normal
// unless `everywhere`, since some scenarios block the whole app.
const OVERVIEW_METHODS: Method[] = ['overview.get', 'why_not_freed.get', 'watch_free.sample']

async function mountOverview(scenario = 'interaction', everywhere = true, transform?: (doc: unknown) => unknown) {
  sessionStorage.clear()
  nav.page = 'overview'
  nav.newScanOpen = false
  const [page, rest] = [fixtureBackend({ scenario }), fixtureBackend({ scenario: everywhere ? scenario : 'normal' })]
  setBackend(async (method, params) => {
    const doc = await (OVERVIEW_METHODS.includes(method) ? page : rest)(method, params)
    return (method === 'overview.get' && transform ? transform(doc) : doc) as never
  })
  const wrapper = mount(App, { attachTo: document.body })
  for (let i = 0; i < 8; i++) {
    await flushPromises()
    await vi.dynamicImportSettled()
  }
  await flushPromises()
  return wrapper
}

/** The normal scenario, with volumes.list changed as the test needs. */
async function mountOverviewWithVolumes(transform: (doc: VolumesDocument) => VolumesDocument) {
  sessionStorage.clear()
  nav.page = 'overview'
  nav.newScanOpen = false
  const base = fixtureBackend({ scenario: 'normal' })
  setBackend(async (method, params) => {
    const result = await base(method, params)
    return (method === 'volumes.list' ? transform(result as VolumesDocument) : result) as never
  })
  const wrapper = mount(App, { attachTo: document.body })
  for (let i = 0; i < 8; i++) {
    await flushPromises()
    await vi.dynamicImportSettled()
  }
  await flushPromises()
  return wrapper
}

/** As above, recording every method and its params. */
async function mountOverviewCounting(transform: (doc: VolumesDocument) => VolumesDocument) {
  sessionStorage.clear()
  nav.page = 'overview'
  nav.newScanOpen = false
  const calls: { method: Method; params: Record<string, unknown> }[] = []
  const base = fixtureBackend({ scenario: 'normal' })
  setBackend(async (method, params) => {
    calls.push({ method, params: params as Record<string, unknown> })
    const result = await base(method, params)
    return (method === 'volumes.list' ? transform(result as VolumesDocument) : result) as never
  })
  const wrapper = mount(App, { attachTo: document.body })
  for (let i = 0; i < 8; i++) {
    await flushPromises()
    await vi.dynamicImportSettled()
  }
  await flushPromises()
  return { wrapper, calls }
}

/** As above, but the optional why-not-freed call waits on `gate`. */
async function mountOverviewWithWhy(gate: Promise<void>) {
  sessionStorage.clear()
  nav.page = 'overview'
  nav.newScanOpen = false
  const base = fixtureBackend()
  setBackend(async (method, params) => {
    if (method === 'why_not_freed.get') await gate
    return base(method, params)
  })
  const wrapper = mount(App, { attachTo: document.body })
  for (let i = 0; i < 8; i++) {
    await flushPromises()
    await vi.dynamicImportSettled()
  }
  await flushPromises()
  return wrapper
}

test('recorded ext4 overview displays inode allocation semantics and hides snapshots', async () => {
  const wrapper = await mountOverview('ext4')
  expect(wrapper.text()).toContain('ext4')
  expect(wrapper.text()).toContain('Inode allocations add up to')
  expect(wrapper.text()).not.toContain('Snapshot-only')
  expect(wrapper.text()).not.toContain('Btrfs data used')
  expect(wrapper.find('[data-testid="btrfs-note"]').exists()).toBe(false)
  expect(wrapper.find('.watch-button').exists()).toBe(false)
  wrapper.unmount()
})

test('explains the writable space by the volume filesystem, not always Btrfs', async () => {
  const wrapper = await mountOverview('ext4')
  const text = wrapper.text()
  expect(text).toContain('ext4 reserves part of the free space for the system')
  expect(text).not.toContain('Btrfs keeps part of the free space in reserve')
  wrapper.unmount()
})

test('a volume without cleanup never promises a cleanup', async () => {
  const wrapper = await mountOverview('ext4', true, doc => {
    const overview = doc as { results: { report: { stale: boolean } }[] }
    overview.results[0]!.report.stale = true
    return overview
  })
  const text = wrapper.text()
  expect(text).not.toContain('Space you can free')
  expect(text).not.toContain('sure to free')
  expect(text).not.toContain('Sure to free')
  expect(text).not.toContain('Free space after cleanup')
  expect(text).not.toContain('re-checked before cleanup')
  wrapper.unmount()
})

// The header badge already says when the scan ran and warns when it is old.
test('a stale report keeps its age in the header badge, without a last-scan row or a Rescan button', async () => {
  const wrapper = await mountOverview('interaction', true, doc => {
    const overview = doc as { results: { report: { stale: boolean } }[] }
    overview.results[0]!.report.stale = true
    return overview
  })
  const badge = wrapper.getComponent(ScanBadge)
  expect(badge.props('report').stale).toBe(true)
  expect(wrapper.text()).not.toContain('Last scanned')
  expect(wrapper.findAll('button').map(button => button.text().trim())).not.toContain('Rescan')
  wrapper.unmount()
})

test.each([
  ['rename_noreplace_unavailable', "This NAS's DSM version can't move files safely, which cleanup needs."],
  ['cleanup_state_unavailable', "Syno Reclaim can't use its own cleanup folder on this volume."],
  ['cleanup_probe_failed', 'The safety check for this volume failed.'],
])('explains the per-volume ext4 refusal %s in plain words, in the space notes', async (reason, message) => {
  const { wrapper } = await mountOverviewCounting(doc => {
    for (const volume of doc.volumes) {
      volume.filesystem = 'ext4'
      volume.capabilities = { scan: true, snapshots: false, reflinks: false, cleanup: false, cleanup_unavailable_reason: reason }
    }
    return doc
  })
  // No callout of its own: the reason waits in the space panel's notes.
  expect(wrapper.find('[data-testid="cleanup-unavailable"]').exists()).toBe(false)
  const notes = wrapper.get('.space-panel [data-testid="scan-notes"]')
  expect(notes.text()).toContain(`${message.replace(/\.$/, "")} · To free space here, delete files in File Station`)
  expect(wrapper.text()).not.toContain('kernel')
  expect(wrapper.findAllComponents(Callout).some(callout => callout.text().includes('Cleanup'))).toBe(false)
  wrapper.unmount()
})

test('ext4 remains browsable and explains cleanup refusal without Btrfs panels', async () => {
  const { wrapper, calls } = await mountOverviewCounting(doc => {
    for (const volume of doc.volumes) {
      volume.filesystem = 'ext4'
      volume.capabilities = { scan: true, snapshots: false, reflinks: false, cleanup: false, cleanup_unavailable_reason: 'ext4_read_only' }
    }
    return doc
  })
  expect(wrapper.get('.space-panel [data-testid="scan-notes"]').text()).toContain('Syno Reclaim only reads ext4 volumes')
  expect(calls.some(call => call.method === 'why_not_freed.get')).toBe(false)
  expect(wrapper.find('.watch-button').exists()).toBe(false)
  wrapper.unmount()
})

test('the volume card warns about a scan that was not fully read, as the header badge does', async () => {
  const wrapper = await mountOverview('normal')
  const card = wrapper.findAllComponents(VolumeCard).find(item => item.props('name') === 'volume1')!
  expect(card.props('chip')).toContain('Not fully read')
  expect(card.props('chipTone')).toBe('warning')
  expect(wrapper.findComponent(ScanBadge).findComponent(PageStatus).props('tone')).toBe('warning')
  wrapper.unmount()
})

// Mounting App.vue needs the startup gate ready; the gate itself is tested separately.
allowAuthorizationForTests()

// Date-only fixture clocks must not leak into later tests.
afterEach(() => vi.useRealTimers())

describe('OverviewPage', () => {
  it('keeps a completed live scan usable and explains approximation in its status', async () => {
    const wrapper = await mountOverview('namespace_churn_many', true, doc => {
      const value = doc as { completion: string; results?: { report?: { attribution?: Record<string, unknown> } }[] }
      if (value.results?.[0]?.report?.attribution) {
        value.completion = 'complete'
        value.results[0].report.attribution.approximation = { changed_subvolumes: 5, skipped_roots: 2, unaccounted_bytes: 4096 }
      }
      return value
    })
    expect(wrapper.text()).toContain('Approximate')
    expect(wrapper.text()).not.toContain('Some data could not be read')
    expect(detailTexts(wrapper).some(text => text.includes('5 changed subvolumes, 2 skipped roots'))).toBe(true)
    wrapper.unmount()
  })
  it('keeps small unknown data neutral and its reasons inspectable', async () => {
    const wrapper = await mountOverview('unknown_small')
    expect(wrapper.text()).toContain('Other (small, unattributed)')
    expect(wrapper.text()).not.toContain('Some data could not be read')
    expect(wrapper.text()).not.toContain('Not fully read')
    expect(detailTexts(wrapper).some(text => text.includes(code('unknown_reason', 'shared_tree_owner_unresolved')))).toBe(true)
    wrapper.unmount()
  })

  it('keeps above-threshold unknown details in the existing status, without a separate note', async () => {
    const wrapper = await mountOverview('unknown_large')
    expect(wrapper.find('[data-testid="unattributed-note"]').exists()).toBe(false)
    expect(wrapper.findAllComponents(Callout).some(callout => callout.props('tone') === 'warning')).toBe(false)
    expect(detailTexts(wrapper).some(text => text.includes('5 GB could not be attributed'))).toBe(true)
    wrapper.unmount()
  })

  it('suppresses minor churn below the threshold; keeps genuine gaps in compact status details', async () => {
    const churn = await mountOverview('namespace_churn')
    expect(churn.find('[data-testid="namespace-churn-note"]').exists()).toBe(false)
    expect(document.body.textContent).not.toContain('Not fully read')
    expect(detailTexts(churn).some(text => text.includes('item changed during the scan and was skipped'))).toBe(false)
    churn.unmount()
    for (const scenario of ['namespace_churn_many', 'coverage_gap', 'partial']) {
      const wrapper = await mountOverview(scenario)
      expect(wrapper.findAllComponents(Callout).some(callout => callout.props('tone') === 'warning')).toBe(false)
      expect(wrapper.find('.volume-card').text()).toContain('Not fully read')
      expect(detailTexts(wrapper).some(text => text.includes('Some data could not be read'))).toBe(true)
      wrapper.unmount()
    }
  })

  it('keeps a legacy one-entry report uncertain without allocating a coverage block', async () => {
    const wrapper = await mountOverview('namespace_churn', true, doc => {
      const result = doc as { results: { report: { attribution: { namespace_churn?: unknown }; coverage: { unreadable_roots?: number } } }[] }
      delete result.results[0]!.report.attribution.namespace_churn
      delete result.results[0]!.report.coverage.unreadable_roots
      return result
    })
    expect(wrapper.find('.volume-card').text()).toContain('Not fully read')
    expect(wrapper.findAllComponents(Callout).some(callout => callout.props('tone') === 'warning')).toBe(false)
    expect(detailTexts(wrapper).some(text => text.includes('Some data could not be read'))).toBe(true)
    wrapper.unmount()
  })

  it('still renders an error when a failed scan has a report and only small unknown bytes', async () => {
    const wrapper = await mountOverview('unknown_small', true, doc => ({ ...(doc as object), completion: 'failed' }))
    expect(wrapper.findAllComponents(Callout).some(callout => callout.props('tone') === 'danger')).toBe(true)
    expect(wrapper.find('.space-panel').exists()).toBe(false)
    wrapper.unmount()
  })

  it.each(['paused', 'cancelled', 'runtime_limit', 'resource_limit'])('keeps a %s scan warning prominent', async completion => {
    const wrapper = await mountOverview('unknown_small', true, doc => ({ ...(doc as object), completion }))
    expect(wrapper.findAllComponents(Callout).some(callout => callout.props('tone') === 'warning')).toBe(true)
    wrapper.unmount()
  })

  it('keeps report read errors prominent even with small unknown bytes', async () => {
    const wrapper = await mountOverview('unknown_small', true, doc => ({ ...(doc as object), errors: [{ code: 'unreadable', message: 'read failed' }] }))
    expect(wrapper.findAllComponents(Callout).some(callout => callout.props('tone') === 'warning')).toBe(true)
    wrapper.unmount()
  })

  it('opens coverage details below the existing status so DSM chrome cannot clip them', async () => {
    const wrapper = await mountOverview('partial')
    const details = wrapper.findAllComponents(Tooltip).filter(tooltip => String(tooltip.props('content')).includes('Some data could not be read'))
    expect(details.length).toBeGreaterThan(0)
    expect(details.every(tooltip => tooltip.props('side') === 'bottom')).toBe(true)
    wrapper.unmount()
  })

  it('shows the volume body while the why explanations still load, and says so', async () => {
    let releaseWhy = () => {}
    const gate = new Promise<void>(resolve => (releaseWhy = resolve))
    const wrapper = await mountOverviewWithWhy(gate)

    // The body does not wait for the optional call.
    expect(wrapper.find('.space-panel').exists()).toBe(true)
    expect(wrapper.find('.actions-head').text()).toContain('Checking why deleted space did not come back…')

    releaseWhy()
    await vi.waitFor(() => expect(wrapper.find('.actions-head').text()).not.toContain('Checking why deleted space did not come back…'))
    expect(wrapper.find('.actions-head').text()).not.toContain('Including why deleted space did not come back')
    wrapper.unmount()
  })

  it('keeps the volume body and says the explanations stay unknown when why-not-freed fails', async () => {
    sessionStorage.clear()
    nav.page = 'overview'
    nav.newScanOpen = false
    const base = fixtureBackend()
    setBackend(async (method, params) => {
      if (method === 'why_not_freed.get') throw new ApiError('cli_error')
      return base(method, params)
    })
    const wrapper = mount(App, { attachTo: document.body })
    for (let i = 0; i < 8; i++) {
      await flushPromises()
      await vi.dynamicImportSettled()
    }
    await flushPromises()
    expect(wrapper.find('.space-panel').exists()).toBe(true)
    expect(wrapper.find('.actions-head').text()).toContain('Could not check why deleted space did not come back')
    wrapper.unmount()
  })
  it('gives packages their own donut tab and opens the Packages page from "View all packages"', async () => {
    const wrapper = await mountOverview()
    expect(wrapper.findAll('.space-panel button').some(button => button.text().includes('View all packages'))).toBe(false)
    const tab = wrapper.findAll('button').find(b => b.text() === 'Packages' && b.attributes('data-ui-tab-value') !== undefined)!
    await tab.trigger('mousedown', { button: 0 })
    await tab.trigger('click')
    await flushPromises()
    const legend = wrapper.find('.space-panel .legend')
    expect(legend.text()).toContain('Synology Drive')
    expect(legend.text()).not.toContain('Files in shared folders')
    expect(wrapper.findAll('.space-panel circle').length).toBe(legend.findAll('li').length)
    const viewAll = wrapper.findAll('.space-panel button').find(button => button.text().includes('View all packages'))
    await viewAll!.trigger('click')
    expect(nav.page).toBe('packages')
    wrapper.unmount()
  })
  // Seen on a test NAS: one "can free" total next to 1.6 TB of package data read as
  // all there was to free. Each source is a suggested row; the card shows free space.
  it('shows the card, donut, reconciliation and actions, without a single "can free" total', async () => {
    const wrapper = await mountOverview()
    const text = wrapper.text()
    expect(text).not.toContain('Space you can free')
    expect(text).not.toContain('Free space after cleanup')
    expect(wrapper.findAll('.hero')).toHaveLength(0)
    // The card uses the same report as the donut, not the later volumes.list capture.
    const card = wrapper.find('.volume-card')
    expect(card.text()).toContain('Used when scanned: 9.6 MB of 512 MB')
    expect(card.text()).not.toContain('Can free')
    expect(card.find('.volume-card-figure').text()).toBe('Free502.4 MB')
    const notes = wrapper.find('.space-panel [data-testid="scan-notes"]')
    expect(notes.text()).toContain('Writable now: 427.8 MB. Btrfs keeps part of the free space in reserve')
    expect(notes.text()).toContain('File data matches Btrfs data used: the parts add up to 3.2 MB, a difference of 0 B')
    expect(text).toContain('Where the space went')
    expect(text).toContain('Files in shared folders')
    expect(wrapper.find('.legend').text()).toContain('Other used (df remainder)6.4 MB')
    // Each legend row gives the part's share of the used space; free space its share of the volume.
    expect(wrapper.find('.legend').text()).toMatch(/Other used \(df remainder\)6\.4 MB\d+(\.\d)?%/)
    expect(wrapper.find('.donut-centre').text()).toBe('9.6 MBusedof 512 MB')
    expect(text).toContain('Synology Drive')
    expect(text).toContain('Free it in Synology Drive Admin Console')
    expect(wrapper.findAll('tbody [data-ui-cell-primary]').map(cell => cell.text())).toContain('Synology Drive')
    expect(wrapper.findAll('circle').length).toBeGreaterThan(3)
    wrapper.unmount()
  })

  it('draws the chart, its legend and the suggested cleanup from one breakdown', async () => {
    const wrapper = await mountOverview('normal')
    const legend = new Map(wrapper.findAll('.space-panel .legend li').map(li => [li.find('[data-ui-chart-legend-label]').text(), li]))
    // DSM service data is split into its large services, the small ones grouped.
    expect([...legend.keys()]).toEqual(expect.arrayContaining(['Container Manager', 'iSCSI LUNs', 'Other DSM service data']))
    expect(legend.has('DSM service data')).toBe(false)
    expect(wrapper.findAll('.space-panel circle')).toHaveLength(legend.size)
    const rows = wrapper.findAll('tbody tr[data-row-key]')
    const row = (title: string) => rows.find(tr => tr.find('[data-ui-cell-primary]').text() === title)!
    // A row and its part in the chart: one amount with one bound, one colour.
    for (const [title, label] of [
      ['Container Manager', 'Container Manager'],
      ['iSCSI LUNs', 'iSCSI LUNs'],
      ['Other DSM service data', 'Other DSM service data'],
      ['Old snapshots', 'Held only by snapshots'],
      ['Recycle Bin', 'Recycle Bins'],
    ]) {
      const li = legend.get(label)!
      expect({ title, size: row(title).find('.size').text() }).toEqual({ title, size: li.get('[data-ui-chart-legend-value] > span').text() })
      expect({ title, color: row(title).find('.swatch').attributes('style') }).toEqual({ title, color: li.find('[data-ui-chart-legend-swatch]').attributes('style') })
    }
    // Snapshots the scan did not read: at least, in both places.
    expect(row('Old snapshots').find('.size').text()).toMatch(/^At least /)
    expect(wrapper.text()).not.toMatch(/Up to 24/)
    wrapper.unmount()
  })

  it('shows the live usage apart from the scanned usage', async () => {
    const wrapper = await mountOverview()
    // volumes.list is the current reading; the card and donut show the report's own df.
    const notes = wrapper.find('.space-panel [data-testid="scan-notes"]')
    expect(notes.text()).toContain('Used now (live): 9.6 MB of 512 MB')
    wrapper.unmount()
  })

  it('lists package data the report ranked out of its actions, and opens it on the Packages page', async () => {
    const wrapper = await mountOverview('interaction', true, original => {
      const doc = structuredClone(original) as { results: { actions: { service?: string }[]; parts: { kind: string; bytes: number; items?: { service?: string; bytes: number }[] }[] }[] }
      const result = doc.results[0]!
      result.actions = result.actions.filter(action => action.service !== 'active_backup_data')
      const dsm = result.parts.find(part => part.kind === 'dsm_service')!
      dsm.items!.find(item => item.service === 'active_backup_data')!.bytes = 1_600_000_000_000
      dsm.bytes += 1_600_000_000_000 - 131072
      return doc
    })
    // The Packages page takes the request as it opens.
    const requests: unknown[] = []
    const stop = watch(() => nav.packageRequest, request => request && requests.push({ ...request }), { flush: 'sync' })
    const row = wrapper.findAll('tbody tr').find(tr => tr.text().includes('Active Backup for Business (backup data)'))!
    // The measured size of its data, as in the chart's legend: no "Up to".
    expect(row.find('.size').text()).toBe('1.5 TB')
    expect(wrapper.findAll('.legend li').find(li => li.text().includes('Active Backup for Business (backup data)'))!.text()).toContain('1.5 TB')
    expect(row.text()).toContain('Free it in Active Backup for Business')
    // The donut counts shared data once; Packages and Explore show the package's whole size (seen on a test NAS: 1.4 vs 1.5 TB).
    // Said once for all package rows, not in each.
    expect(row.text()).not.toContain('The Packages page can show a larger size')
    expect(wrapper.find('[data-testid="package-note"]').text()).toContain('The Packages page can show a larger size: it also counts data the package shares with folders')
    await row.findAll('button').find(button => button.text() === 'Open in Packages')!.trigger('click')
    stop()
    expect(nav.page).toBe('packages')
    expect(requests).toEqual([{ service: 'active_backup_data', path: '' }])
    wrapper.unmount()
  })

  // Seen on a test NAS: single ISO files were called "Large folder".
  it('names a large file as a file and a large folder as a folder', async () => {
    const wrapper = await mountOverview('interaction', true, original => {
      const doc = structuredClone(original) as { results: { actions: Record<string, unknown>[] }[] }
      const cleanup = (rank: number, path: string, path_kind: string) => ({
        rank, kind: 'cleanup', share: 'Misc', reclaimable_bytes: 9_000_000_000 - rank, held_bytes: null, manage_in_id: 'file_station', path: { path }, path_kind,
      })
      doc.results[0]!.actions = [cleanup(1, '/volume1/Misc/disk.iso', 'file'), cleanup(2, '/volume1/Misc/old', 'dir')]
      return doc
    })
    const titleOf = (path: string) => wrapper.findAll('tbody tr').find(tr => tr.text().includes(path))!.find('[data-ui-cell-primary]').text()
    expect(titleOf('/volume1/Misc/disk.iso')).toBe('Large file')
    expect(titleOf('/volume1/Misc/old')).toBe('Large folder')
    wrapper.unmount()
  })

  it('explains in plain words why deleting on Btrfs may free less', async () => {
    const wrapper = await mountOverview()
    const note = wrapper.find('.space-panel [data-testid="btrfs-note"]')
    expect(note.text()).toContain('can share stored data')
    expect(note.text()).not.toMatch(/reflink|extent|subvolume/i)
    wrapper.unmount()
  })

  it('lists suggested cleanup with what each is, why it uses space and one named action', async () => {
    const wrapper = await mountOverview()
    const head = wrapper.find('.actions-head').text()
    expect(head).toContain('Suggested cleanup')
    const headers = wrapper.findAll('thead th').map(th => th.text())
    expect(headers).not.toContain('Status')
    expect(headers).not.toContain('Why')
    const buttons = wrapper.findAll('tbody button').map(button => button.text())
    // Package data opens its package on the Packages page, which says how to free it.
    expect(buttons).toContain('Open in Packages')
    expect(buttons).not.toContain('How')
    expect(wrapper.find('[data-testid="package-note"]').text()).toContain('Packages keep this data to work.')
    // Every row has one icon and at most one short line under its name.
    for (const row of wrapper.findAll('tbody tr[data-row-key]')) expect(row.findAll('td')[0].findAll('svg').length).toBe(1)
    wrapper.unmount()
  })

  it('shows the captured volume report as partial', async () => {
    const wrapper = await mountOverview('normal')
    expect(wrapper.find('.volume-card').text()).toContain('Not fully read')
    expect(detailTexts(wrapper).some(text => text.includes('Some data could not be read'))).toBe(true)
    wrapper.unmount()
  })

  it('gives the report age in the volume chip, as the other pages do', async () => {
    // The recorded report is a fixed timestamp: pin now so "ago" does not turn
    // into "yesterday" once the fixture is a day old.
    vi.useFakeTimers({ toFake: ['Date'] })
    vi.setSystemTime(new Date('2026-09-29T10:00:00Z'))
    const wrapper = await mountOverview()
    expect(wrapper.find('.volume-card').text()).toMatch(/(?:ago|yesterday|today) · No changes seen/)
    expect(wrapper.find('.volume-card').text()).not.toContain('2026')
    wrapper.unmount()
    vi.useRealTimers()
  })

  it('loads only the shown volume and keeps the others compact above four volumes', async () => {
    volumes.selected = '/volume1'
    const { wrapper, calls } = await mountOverviewCounting(doc => {
      const first = doc.volumes[0]
      return {
        ...doc,
        volumes: Array.from({ length: 8 }, (_, index) => ({
          ...first,
          mount: { ...first.mount, path: `/volume${index + 1}` },
          shares: [],
          reports: index === 0 ? 1 : 0,
        })),
      }
    })
    const requested = calls.filter(call => call.method === 'overview.get').map(call => call.params.volume)
    expect(new Set(requested)).toEqual(new Set(['/volume1']))
    expect(wrapper.findAll('.volume-card')).toHaveLength(8)
    expect(wrapper.findAll('.volume-card--compact')).toHaveLength(7)
    expect(wrapper.findAll('.volume-card')[0].classes()).not.toContain('volume-card--compact')
    wrapper.unmount()
  })

  it('switches the donut to shared folders', async () => {
    const wrapper = await mountOverview()
    const tab = wrapper.findAll('button').find(b => b.text() === 'By shared folder')!
    await tab.trigger('mousedown', { button: 0 })
    await tab.trigger('click')
    await flushPromises()
    expect(wrapper.find('.legend').text()).toContain('share04')
    wrapper.unmount()
  })

  it('asks for a scan when the volume has no report', async () => {
    const wrapper = await mountOverview('no_report')
    expect(wrapper.text()).toContain('Not scanned yet')
    wrapper.unmount()
  })

  /** Overview calls and volumes follow `scenario`; the jobs are those of a running first scan. */
  async function mountWithRunningScan(scenario: string, everywhere = true) {
    sessionStorage.clear()
    nav.page = 'overview'
    nav.newScanOpen = false
    const [page, running] = [fixtureBackend({ scenario }), fixtureBackend({ scenario: 'scanning' })]
    setBackend(async (method, params) => (method === 'jobs.list' ? running : everywhere || OVERVIEW_METHODS.includes(method) ? page : fixtureBackend({ scenario: 'normal' }))(method, params))
    const wrapper = mount(App, { attachTo: document.body })
    for (let i = 0; i < 8; i++) {
      await flushPromises()
      await vi.dynamicImportSettled()
    }
    await flushPromises()
    return wrapper
  }

  it('reports a running first scan instead of asking for one', async () => {
    const wrapper = await mountWithRunningScan('no_report')
    expect(wrapper.text()).not.toContain('Not scanned yet')
    expect(wrapper.text()).toContain('Scanning this volume')
    expect(wrapper.text()).toContain('5,371,562 entries read')
    expect(wrapper.findAll('button').some(b => b.text() === 'Scan now')).toBe(false)
    await wrapper.findAll('button').find(b => b.text() === 'View progress')!.trigger('click')
    expect(nav.page).toBe('scanning')
    wrapper.unmount()
  })

  it('shows one clear status on the card of a volume scanned for the first time', async () => {
    const wrapper = await mountWithRunningScan('no_report')
    expect(wrapper.find('.volume-card-head').text()).toContain('Scanning')
    expect(wrapper.find('.volume-card-head').text()).not.toContain('unknown')
    wrapper.unmount()
  })

  it('keeps showing the previous result while a rescan runs', async () => {
    const wrapper = await mountWithRunningScan('normal')
    expect(wrapper.text()).not.toContain('Scanning this volume')
    expect(wrapper.find('.space-panel').exists()).toBe(true)
    wrapper.unmount()
  })

  it('does not offer a new scan from the whole-volume prompt while one runs', async () => {
    const wrapper = await mountWithRunningScan('no_report', false)
    expect(wrapper.text()).toContain('Overview needs a whole-volume scan')
    expect(wrapper.findAll('button').some(b => b.text() === 'Scan now')).toBe(false)
    expect(wrapper.findAll('button').some(b => b.text() === 'View progress')).toBe(true)
    wrapper.unmount()
  })

  const changedTooltip = (wrapper: ReturnType<typeof mount>) =>
    detailTexts(wrapper).find(text => text.includes('Data changed while it was scanned:')) ?? ''

  it('explains that a change only in system areas does not affect shared-folder cleanup', async () => {
    const wrapper = await mountOverview('changed_system')
    expect(pageStatusText()).toContain('Changed during scan')
    const content = changedTooltip(wrapper)
    expect(content).toContain('Changes were only in system areas')
    expect(content).toContain('A subvolume was written to')
    wrapper.unmount()
  })

  it('keeps the plain wording when a shared folder changed', async () => {
    const wrapper = await mountOverview('changed_share')
    const content = changedTooltip(wrapper)
    expect(content).toContain('Data changed while it was scanned:')
    expect(content).not.toContain('Changes were only in system areas')
    wrapper.unmount()
  })

  it('shows the system-areas note without hovering', async () => {
    const wrapper = await mountOverview('changed_system')
    expect(wrapper.find('.system-areas-note').text()).toBe(
      "Changes were only in system areas (DSM's own writes); cleanup in shared folders is not affected",
    )
    wrapper.unmount()
  })

  it('has no system-areas note when a shared folder changed', async () => {
    const wrapper = await mountOverview('changed_share')
    expect(wrapper.find('.system-areas-note').exists()).toBe(false)
    wrapper.unmount()
  })

  it('asks for a whole-volume scan when the volume only has folder reports', async () => {
    const wrapper = await mountOverview('no_report', false)
    expect(wrapper.text()).toContain('Overview needs a whole-volume scan')
    expect(wrapper.text()).toContain('Folder scans can still be explored in Explore')
    wrapper.unmount()
  })

  it('opens Explore for the folder reports of a volume scanned only by share', async () => {
    const wrapper = await mountOverview('share_scan')
    expect(wrapper.text()).toContain('Overview needs a whole-volume scan')
    await wrapper.findAll('button').find(button => button.text() === 'Open Explore')!.trigger('click')
    expect(nav.page).toBe('explore')
    wrapper.unmount()
  })

  it('warns about partial reports with the unknown reasons', async () => {
    const wrapper = await mountOverview('partial')
    expect(detailTexts(wrapper).some(text => text.includes('Memory limit reached while resolving owners'))).toBe(true)
    expect(wrapper.find('.volume-card').text()).toContain('Not fully read')
    wrapper.unmount()
  })

  it.each([
    ['busy', 'Another scan is using this volume'],
    ['error', 'The Syno Reclaim command failed'],
  ])('shows the %s state', async (scenario, message) => {
    const wrapper = await mountOverview(scenario, false)
    expect(wrapper.text()).toContain(message)
    wrapper.unmount()
  })

  it('shows a paused scan on the card and leads to the scan page', async () => {
    vi.useFakeTimers({ toFake: ['Date'] })
    vi.setSystemTime(new Date('2026-09-30T01:04:20Z'))
    const wrapper = await mountOverview('paused')
    expect(wrapper.text()).toContain('Paused at 35%')
    await wrapper.findAll('button').find(b => b.text() === 'View scan')!.trigger('click')
    expect(nav.page).toBe('scanning')
    wrapper.unmount()
  })

  it('opens the watch-free sheet', async () => {
    const wrapper = await mountOverview()
    await wrapper.findAll('button').find(b => b.text().includes('Watch free space'))!.trigger('click')
    await flushPromises()
    expect(document.body.textContent).toContain('This measures the free space of /volume1 every 2 seconds')
    wrapper.unmount()
  })

  // Seen on a test NAS: the System partition card already says Full and how much is
  // free, so a banner above the volumes repeated it. The card carries the way in.
  it('puts the getting-full warning and its way in on the System partition card, without a banner', async () => {
    const wrapper = await mountOverview('sysvol_getting_full')
    expect(wrapper.find('.rootfs-banner').exists()).toBe(false)
    expect(wrapper.findAllComponents(Callout).some(c => c.text().includes('getting full'))).toBe(false)
    const card = wrapper.findAll('.volume-card').at(-1)!
    expect(card.text()).toContain('System partition')
    expect(card.text()).toContain('Getting full')
    expect(card.text()).toContain('7.1 GB of 8 GB used')
    expect(card.text()).toContain('Free')
    expect(card.text()).toContain('886.9 MB')
    expect(card.find('.volume-card-action').text()).toContain('Check and clean up')
    expect(wrapper.findAllComponents(VolumeCard).at(-1)!.props('chipDetails')).toBe('Clean up before the next DSM update')
    expect(wrapper.text()).not.toContain('System partition is full')
    await card.trigger('click')
    expect(wrapper.findAll('.volume-card').at(-1)!.classes()).toContain('selected')
    wrapper.unmount()
  })

  it('puts the full warning and Clean up now on the System partition card, without a banner', async () => {
    const wrapper = await mountOverview('sysvol_full')
    expect(wrapper.find('.rootfs-banner').exists()).toBe(false)
    const card = wrapper.findAll('.volume-card').at(-1)!
    expect(card.text()).toContain('System partition')
    expect(card.text()).toContain('Full')
    expect(card.text()).toContain('7.6 GB of 8 GB used')
    expect(card.text()).toContain('362.4 MB')
    expect(card.find('.volume-card-action').text()).toContain('Clean up now')
    expect(wrapper.findAllComponents(VolumeCard).at(-1)!.props('chipDetails')).toContain('DSM updates may fail')
    wrapper.unmount()
  })

  it('adds the Full actions row with the movable size and leads to the System partition card', async () => {
    const wrapper = await mountOverview('sysvol_full')
    const table = wrapper.find('table')
    expect(table.text()).toContain('System partition is full')
    expect(table.text()).toContain('DSM updates may fail. Syno Reclaim can move 110 MB off it')
    expect(table.text()).toContain('110 MB')
    await table.findAll('button').find(b => b.text() === 'Clean up')!.trigger('click')
    expect(wrapper.findAll('.volume-card').at(-1)!.classes()).toContain('selected')
    wrapper.unmount()
  })

  it('sizes the Full actions row by what a deep cleanup can move', async () => {
    const wrapper = await mountOverview('sysvol_deep_short')
    const table = wrapper.find('table')
    expect(table.text()).toContain('DSM updates may fail. Syno Reclaim can move up to 247.6 MB off it')
    expect(table.text()).toContain('Up to 247.6 MB')
    wrapper.unmount()
  })

  it('swaps the Overview body for the System partition view when its card is selected (S3)', async () => {
    // Pin the clock: the fixture's report is a fixed timestamp, and "ago" must not
    // turn into "yesterday" depending on the day the suite runs.
    vi.useFakeTimers({ toFake: ['Date'] })
    vi.setSystemTime(new Date('2026-10-03T00:30:06Z'))
    const wrapper = await mountOverview('sysvol_getting_full')
    const toolbarStatus = pageStatusText()
    await wrapper.findAll('.volume-card').at(-1)!.trigger('click')
    await flushPromises()
    const view = wrapper.find('.rootfs-view')
    expect(view.exists()).toBe(true)
    await vi.waitFor(() => expect(view.text()).toContain('What uses the space'))
    expect(view.text()).toContain('Can be moved off it')
    expect(wrapper.find('.space-panel').exists()).toBe(false)
    // Switching views keeps the volume's status, regardless of relative date wording.
    expect(pageStatusText()).toBe(toolbarStatus)
    expect(pageStatusText()).toContain('Not fully read')
    wrapper.unmount()
    vi.useRealTimers()
  })

  it('shows no Full row or card action with the warning off, but keeps the System partition card', async () => {
    setSystemPartitionWarning(false)
    try {
      const wrapper = await mountOverview('sysvol_full')
      expect(wrapper.find('table').text()).not.toContain('System partition is full')
      const card = wrapper.findAll('.volume-card').at(-1)!
      expect(card.text()).toContain('System partition')
      expect(card.find('.volume-card-action').exists()).toBe(false)
      await card.trigger('click')
      await flushPromises()
      expect(wrapper.find('.rootfs-view').exists()).toBe(true)
      wrapper.unmount()
    } finally {
      localStorage.clear()
      readPreferences()
    }
  })

  it('draws the System partition card in neutral colours with the warning off', async () => {
    setSystemPartitionWarning(false)
    try {
      for (const scenario of ['sysvol_full', 'sysvol_getting_full']) {
        const wrapper = await mountOverview(scenario)
        const card = wrapper.findAllComponents(VolumeCard).at(-1)!
        expect(card.props('name')).toBe('System partition')
        expect([card.props('chipTone'), card.props('barTone')]).toEqual(['neutral', 'primary'])
        wrapper.unmount()
      }
    } finally {
      localStorage.clear()
      readPreferences()
    }
  })

  it('keeps the System partition card warning colours with the warning on', async () => {
    const full = await mountOverview('sysvol_full')
    const card = full.findAllComponents(VolumeCard).at(-1)!
    expect([card.props('chipTone'), card.props('barTone')]).toEqual(['danger', 'danger'])
    full.unmount()
  })

  it('opens the System partition view from its card', async () => {
    const wrapper = await mountOverview('sysvol_getting_full')
    await wrapper.findAll('.volume-card').at(-1)!.trigger('click')
    await flushPromises()
    expect(wrapper.find('.rootfs-view').exists()).toBe(true)
    wrapper.unmount()
  })

  // Seen on a test NAS: a just-scanned volume read "No results yet" with Loading
  // below while the System partition walk held the one query slot.
  it('reads the System partition only after the volume overview has answered', async () => {
    sessionStorage.clear()
    nav.page = 'overview'
    nav.newScanOpen = false
    const base = fixtureBackend({ scenario: 'sysvol_full' })
    let release!: () => void
    const gate = new Promise<void>(resolve => { release = resolve })
    const calls: Method[] = []
    setBackend(async (method, params) => {
      calls.push(method)
      if (method === 'overview.get') await gate
      return base(method, params)
    })
    const wrapper = mount(App, { attachTo: document.body })
    for (let i = 0; i < 8; i++) {
      await flushPromises()
      await vi.dynamicImportSettled()
    }
    expect(calls).toContain('overview.get')
    expect(calls).not.toContain('rootfs.report')
    const card = wrapper.findAll('.volume-card')[0]!
    expect(card.text()).toContain('Free')
    expect(card.text()).not.toContain('No results yet')
    release()
    for (let i = 0; i < 8; i++) {
      await flushPromises()
      await vi.dynamicImportSettled()
    }
    expect(calls).toContain('rootfs.report')
    expect(wrapper.find('.space-panel').exists()).toBe(true)
    wrapper.unmount()
  })

  it('reads the System partition at once when its card is selected', async () => {
    sessionStorage.clear()
    nav.page = 'overview'
    nav.newScanOpen = false
    const base = fixtureBackend({ scenario: 'sysvol_full' })
    const calls: Method[] = []
    setBackend(async (method, params) => {
      calls.push(method)
      if (method === 'overview.get') return new Promise<never>(() => {})
      return base(method, params)
    })
    const wrapper = mount(App, { attachTo: document.body })
    for (let i = 0; i < 8; i++) {
      await flushPromises()
      await vi.dynamicImportSettled()
    }
    await wrapper.findAll('.volume-card').at(-1)!.trigger('click')
    await flushPromises()
    expect(calls).toContain('rootfs.report')
    wrapper.unmount()
  })

  it('updates the System partition card once Check again has read the partition', async () => {
    sessionStorage.clear()
    nav.page = 'overview'
    nav.newScanOpen = false
    const base = fixtureBackend({ scenario: 'sysvol_getting_full' })
    let checked = false
    setBackend(async (method, params) => {
      if (method === 'rootfs.report' && (params as { max_age_s?: number }).max_age_s === 0) checked = true
      const result = await base(method, params)
      if (method !== 'volumes.list' || !checked) return result
      const doc = structuredClone(result) as VolumesDocument
      doc.system_partition!.available_bytes = 700_000_000
      return doc as never
    })
    const wrapper = mount(App, { attachTo: document.body })
    for (let i = 0; i < 8; i++) {
      await flushPromises()
      await vi.dynamicImportSettled()
    }
    await wrapper.findAll('.volume-card').at(-1)!.trigger('click')
    await vi.waitFor(() => expect(wrapper.find('.rootfs-refresh').exists()).toBe(true))
    await wrapper.find('.rootfs-refresh').trigger('click')
    await vi.waitFor(() => expect(wrapper.findAll('.volume-card').at(-1)!.text()).toContain('667.6 MB'))
    expect(wrapper.find('.rootfs-view').text()).toContain('What uses the space')
    wrapper.unmount()
  })

  it('says the answer has no result instead of loading forever', async () => {
    const wrapper = await mountOverview('normal', false, doc => ({ ...(doc as object), results: [] }))
    expect(wrapper.find('.loading').exists()).toBe(false)
    expect(wrapper.text()).toContain('The Syno Reclaim command failed')
    expect(wrapper.findAll('button').some(b => b.text() === 'Retry')).toBe(true)
    wrapper.unmount()
  })

  it('returns to the volume body with a toast when the partition is normal again', async () => {
    sessionStorage.clear()
    nav.page = 'overview'
    nav.newScanOpen = false
    const volumesBackend = fixtureBackend({ scenario: 'sysvol_getting_full' })
    const normal = fixtureBackend()
    setBackend(async (method, params) => {
      const backend = method === 'rootfs.report' || method === 'jobs.list' || (method === 'job.result' && String((params as {job_id?: string}).job_id).startsWith('planning-fixture-')) ? normal : volumesBackend
      return (await (backend as typeof volumesBackend)(method, params)) as never
    })
    document.body.innerHTML = ''
    // Module state: a previous test's "full" volumes would preload the report
    // before the click, and the view would then never see it arrive.
    volumes.document = null
    volumes.selected = null
    const wrapper = mount(App, { attachTo: document.body })
    for (let i = 0; i < 8; i++) {
      await flushPromises()
      await vi.dynamicImportSettled()
    }
    await wrapper.findAll('.volume-card').at(-1)!.trigger('click')
    // The report is read lazily; wait until its view has closed with the toast.
    for (let i = 0; i < 50; i++) {
      if (!wrapper.find('.rootfs-view').exists() && document.body.textContent?.includes('The system partition is normal again')) break
      await flushPromises()
      await vi.dynamicImportSettled()
    }
    expect(wrapper.find('.rootfs-view').exists()).toBe(false)
    expect(wrapper.find('.space-panel').exists()).toBe(true)
    expect(document.body.textContent).toContain('The system partition is normal again')
    wrapper.unmount()
  })

  it('shows nothing about the system partition when it is normal, leaving the Overview unchanged', async () => {
    // Ignore generated ids, entrance styles and the temporary new-row animation marker.
    const dom = (wrapper: ReturnType<typeof mount>) =>
      wrapper
        .find('.overview')
        .html()
        .replace(/reka-[\w-]*-v-\d+-\d+/g, 'reka-x')
        .replace(/animation-duration: 0s;?/g, '')
        .replace(/ data-ui-row-new=""/g, '')
        .replace(/ style=""/g, '')
    const wrapper = await mountOverview('normal')
    expect(wrapper.find('.rootfs-banner').exists()).toBe(false)
    expect(wrapper.findAll('.volume-card').some(c => c.text().includes('System partition'))).toBe(false)
    expect(wrapper.text()).not.toContain('System partition is full')
    const unchanged = dom(wrapper)
    wrapper.unmount()

    // An unreadable system partition is like Normal: the DOM does not change (§8).
    const unreadable = await mountOverviewWithVolumes(doc => ({ ...doc, system_partition: null }))
    expect(unreadable.find('.rootfs-banner').exists()).toBe(false)
    expect(dom(unreadable)).toBe(unchanged)
    unreadable.unmount()
  })

  it('says which volumes are skipped for not being Btrfs, without offering them', async () => {
    const wrapper = await mountOverview('unsupported_skipped')
    const note = wrapper.find('[data-testid="unsupported-volumes"]')
    expect(note.exists()).toBe(true)
    expect(note.text()).toContain('/volume2')
    expect(note.text()).toContain('These file systems cannot be scanned')
    expect(wrapper.findAll('.volume-row button').some(card => card.text().includes('volume2'))).toBe(false)
    wrapper.unmount()
  })

  it('shows no skipped-volume note when every volume is Btrfs', async () => {
    const wrapper = await mountOverview('normal')
    expect(wrapper.find('[data-testid="unsupported-volumes"]').exists()).toBe(false)
    wrapper.unmount()
  })
})

test('offers a Gentle deep pass for metadata-read limits without requiring a new namespace scan', async () => {
  const wrapper = await mountOverview('normal', true, doc => {
    const value = doc as { results: Record<string, unknown>[] }
    value.results[0]!.attribution = {
      read_budget: { adaptive: true, used_bytes: 2 ** 40, extent_count: 2_000_000, snapshot_count: 269, required_reads: 0, selected_reads: 6_000_000 },
      deferred_bytes: 200 * 2 ** 30,
      analysis: null,
    }
    return value
  })
  expect(wrapper.text()).toContain('The metadata read limit left')
  expect(wrapper.text()).toContain('Attribute remaining space')
  expect(wrapper.text()).toContain('Low I/O priority')
  wrapper.unmount()
})

test('deep attribution submits the saved report with Gentle mode and follows its job', async () => {
  const wrapper = await mountOverview('normal', true, doc => {
    const value = doc as { results: Record<string, unknown>[] }
    value.results[0]!.attribution = { read_budget: null, deferred_bytes: 200 * 2 ** 30, analysis: null }
    return value
  })
  const calls: { method: Method; params: unknown }[] = []
  const base = fixtureBackend()
  setBackend(async (method, params) => {
    calls.push({ method, params })
    if (method === 'scan.start') return { outcome: 'detached', job_id: 'deep-job' } as never
    return base(method, params)
  })
  const start = wrapper.findAll('button').find(b => b.text() === 'Attribute remaining space')!
  await start.trigger('click')
  await flushPromises()
  expect(calls.find(c => c.method === 'scan.start')?.params).toMatchObject({ mode: 'gentle', attribution_report: expect.any(String), targets: [volumes.document!.volumes[0]!.mount] })
  expect(nav.followedJobId).toBe('deep-job')
  expect(nav.page).toBe('scanning')
  wrapper.unmount()
})
