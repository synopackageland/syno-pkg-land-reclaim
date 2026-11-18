import { enableAutoUnmount, flushPromises, mount, type VueWrapper } from '@vue/test-utils'
import { setBackend } from '@/api'
import { formatBytes } from '@/i18n'
import type { CacheDocument, TopDocument } from '@/types'
import { fixtureBackend } from '@/api/fixtures'
import { apiErrorFromCgi } from '@/api/errors'
import { invalidateAnswers } from '@/stores/answers'
import { jobs } from '@/stores/jobs'
import { nav, openPackage, takeExploreRequest } from '@/stores/nav'
import { loadVolumes, volumes } from '@/stores/volumes'
import { showPageActionsInPlace } from '@/shell/pageActionsInPlace'
import { openPageStatus } from '@/components/pageInfoTesting'
import ScanBadge from '@/components/ScanBadge.vue'
import PackagesPage from './PackagesPage.vue'
import pageSource from './PackagesPage.vue?raw'

const VOLUME = '/volume1'

showPageActionsInPlace()

async function settle() {
  await flushPromises()
  await vi.dynamicImportSettled()
  await flushPromises()
}

// Dispose watchers even if setup or an assertion fails.
enableAutoUnmount(afterEach)

async function mountPage(scenario = 'packages', realTransitions = false): Promise<VueWrapper> {
  setBackend(fixtureBackend({ scenario }))
  volumes.selected = VOLUME
  await loadVolumes()
  const wrapper = mount(PackagesPage, { attachTo: document.body, global: { stubs: { transition: !realTransitions } } })
  await settle()
  return wrapper
}

const rowTexts = (wrapper: VueWrapper) => wrapper.findAll('tbody tr').map(row => row.text())

beforeEach(() => {
  sessionStorage.clear()
  localStorage.clear()
  jobs.list = []
  nav.packageRequest = null
  nav.exploreRequest = null
  document.body.innerHTML = ''
})

describe('Packages page', () => {
  it('does not invent a Reclaim row when the selected report has no Reclaim service data', async () => {
    const base = fixtureBackend({ scenario: 'packages' })
    setBackend(async (method, params) => {
      const answer = await base(method, params)
      if (method !== 'top.list') return answer
      const doc = structuredClone(answer) as TopDocument
      for (const result of doc.results) {
        result.services.lines = result.services.lines.filter(line => line.service !== 'syno_reclaim')
        result.entries = result.entries.filter(entry => entry.service?.id !== 'syno_reclaim')
      }
      return doc as never
    })
    volumes.selected = VOLUME
    await loadVolumes()
    const wrapper = mount(PackagesPage, { attachTo: document.body })
    await settle()
    expect(wrapper.text()).not.toContain('Syno Reclaim (this app)')
    await wrapper.get('button[aria-label="Show details of Container Manager"]').trigger('click')
    await settle()
    expect(wrapper.find('[data-ui-list-detail-pane]').text()).toContain('Container Manager')
    expect(wrapper.findAll('tbody tr').length).toBeGreaterThan(0)
  })
  it('stores keyboard pane resizing independently from the Recycle Bin page', async () => {
    const recycleKey = `syno-pkg-land-ui:detail-width:${location.host}${location.pathname.replace(/\/[^/]*$/, '')}:recycle`
    localStorage.setItem(recycleKey, '512')
    const wrapper = await mountPage()
    await wrapper.get('button[aria-label="Show details of Container Manager"]').trigger('click')
    const separator = wrapper.get('[data-ui-list-detail-separator]')
    await separator.trigger('keydown', { key: 'End' })
    const minimum = Number(separator.attributes('aria-valuemin'))
    expect(Number(separator.attributes('aria-valuenow'))).toBe(minimum)
    const key = Object.keys(localStorage).find(key => key.startsWith('syno-pkg-land-ui:detail-width:') && key.endsWith(':packages'))!
    expect(localStorage.getItem(key)).toBe(String(minimum))
    expect(localStorage.getItem(recycleKey)).toBe('512')
    await wrapper.get('[data-ui-list-detail-close]').trigger('click')
    expect(wrapper.find('[data-ui-list-detail-pane]').exists()).toBe(false)
    wrapper.unmount()
  })

  it('says a system operation failed, with its code, when the volume level fails as root', async () => {
    const backend = fixtureBackend({ scenario: 'packages' })
    setBackend((method, params) => method === 'top.list' ? Promise.reject(apiErrorFromCgi('ROOT_FAILED')) : backend(method, params))
    volumes.selected = VOLUME
    await loadVolumes()
    const wrapper = mount(PackagesPage, { attachTo: document.body })
    await settle()
    expect(wrapper.text()).toContain('A system operation failed (error code: ROOT_FAILED)')
    expect(wrapper.text()).not.toContain('Package Center')
    wrapper.unmount()
  })

  // One volume offers no choice; a lone tab drew an edge-to-edge strip.
  it('shows the volume tabs only when there is more than one volume', async () => {
    const wrapper = await mountPage()
    expect(wrapper.find('.volume-picker').exists()).toBe(false)
    const [first] = volumes.document!.volumes
    volumes.document!.volumes.push({ ...first!, mount: { ...first!.mount, path: '/volume2' } })
    await flushPromises()
    expect(wrapper.findAll('.volume-picker [data-ui-tab-value]').map(tab => tab.text())).toEqual(['volume1', 'volume2'])
    wrapper.unmount()
  })

  it('shows a muted note, not the banner, for attribution-only unknown above the threshold; the banner stays for a coverage gap', async () => {
    const large = await mountPage('unknown_large')
    expect((await openPageStatus()).notes.join('\n')).toContain('could not be attributed and is not counted as reclaimable')
    expect(large.text()).not.toContain('Some data could not be read')
    large.unmount()
    document.body.innerHTML = ''
    const gap = await mountPage('coverage_gap')
    expect(gap.get('[data-testid="partial-hint"]').text()).toContain('Some data could not be read')
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

  it('keeps folder metadata out of the list without a separate callout', async () => {
    const base = fixtureBackend({ scenario: 'packages' })
    setBackend(async (method, params) => {
      const answer = await base(method, params)
      if (method !== 'top.list') return answer
      const doc = structuredClone(answer) as TopDocument
      doc.results[0].services.lines.push({
        ...doc.results[0].services.lines[0],
        service: 'folder_metadata', names: '@eaDir', included_in_rows: true,
      })
      return doc
    })
    volumes.selected = VOLUME
    await loadVolumes()
    const wrapper = mount(PackagesPage, { attachTo: document.body })
    await settle()
    expect(rowTexts(wrapper)).toHaveLength(12)
    expect(wrapper.text()).not.toContain('Thumbnails and file metadata')
    wrapper.unmount()
  })

  it.each([true, false])('shows this app and opens Scan history with cache status available=%s', async available => {
    const base = fixtureBackend({ scenario: 'packages' })
    setBackend(async (method, params) => {
      if (method === 'cache.status' && !available) throw new Error('unavailable')
      const answer = await base(method, params)
      if (method === 'cache.status') {
        const cache = structuredClone(answer) as CacheDocument
        cache.filesystems = [{ ...cache.filesystems[0], bytes: 1024 }]
        return cache
      }
      if (method !== 'top.list') return answer
      const doc = structuredClone(answer) as TopDocument
      doc.results[0].services.lines.push({
        ...doc.results[0].services.lines[0],
        metrics: { ...doc.results[0].services.lines[0].metrics, reclaimable: null, reclaimable_resolved: 0 },
        service: 'syno_reclaim', package: 'Syno Reclaim (this app)',
        names: 'SynoReclaim', manage_in_id: 'syno_reclaim',
      })
      return doc
    })
    volumes.selected = VOLUME
    await loadVolumes()
    const wrapper = mount(PackagesPage, { attachTo: document.body })
    await settle()
    const own = wrapper.findAll('tbody tr').find(row => row.text().includes('Syno Reclaim (this app)'))!
    expect(own.text()).toContain('Clear here')
    await own.find('button').trigger('click')
    await settle()
    const detail = wrapper.find('[data-ui-list-detail-pane]')
    expect(detail.text()).toContain(`Clearable cache: ${formatBytes(available ? 1024 : null)}`)
    const button = detail.findAll('button').find(button => button.text().includes('Scan history'))!
    await button.trigger('click')
    expect(nav.page).toBe('history')
    wrapper.unmount()
  })

  it('lists one row per service line of the volume, largest first', async () => {
    const wrapper = await mountPage()
    const rows = rowTexts(wrapper)
    expect(rows).toHaveLength(12)
    expect(rows[0]).toContain('Container Manager')
    expect(rows[1]).toContain('Installed packages')
    const [name, size] = wrapper.find('tbody tr').findAll('[data-ui-cell-primary]').map(cell => cell.text())
    expect(name).toBe('Container Manager')
    expect(size).toMatch(/ GB$/)
    wrapper.unmount()
  })

  it('keeps the system folder names out of the list and shows them in the detail', async () => {
    const wrapper = await mountPage()
    expect(wrapper.find('[data-ui-list-detail-list]').text()).not.toContain('@')
    const containerRow = wrapper.findAll('tbody tr').find(row => row.text().includes('Container Manager'))!
    expect(containerRow.find('[data-ui-cell-secondary]').exists()).toBe(false)
    await wrapper.find('button[aria-label="Show details of Container Manager"]').trigger('click')
    expect(wrapper.find('[data-ui-list-detail-pane]').text()).toContain('@')
    wrapper.unmount()
  })

  it('says what the space is for and which data the page does not count', async () => {
    const wrapper = await mountPage()
    const text = wrapper.text()
    expect(text).toContain('usually can\'t be deleted directly')
    expect(text).toContain('Package data kept in shared folders')
    expect(text).toContain('is listed here too')
    wrapper.unmount()
  })

  // The intro spans the content area; a 72ch cap left half the window empty.
  it('lets the intro use the width of the content area', () => {
    const intro = /\.intro\s*\{([^}]*)\}/.exec(pageSource)?.[1]
    expect(intro).toBeDefined()
    expect(intro).not.toMatch(/max-width/)
  })

  // The header badge carries an old report's age; the notes open beside the title.
  it('keeps a stale report in the header badge, without a last-scan row or a notes line', async () => {
    const base = fixtureBackend({ scenario: 'packages' })
    setBackend(async (method, params) => {
      const answer = await base(method, params)
      if (method === 'top.list') (answer as TopDocument).results[0].report.stale = true
      return answer
    })
    volumes.selected = VOLUME
    await loadVolumes()
    const wrapper = mount(PackagesPage, { attachTo: document.body })
    await settle()
    expect(wrapper.getComponent(ScanBadge).props('report').stale).toBe(true)
    expect(wrapper.text()).not.toContain('Last scanned')
    expect(wrapper.find('[data-testid="scan-notes"]').exists()).toBe(false)
    wrapper.unmount()
  })

  it('lists package data kept in shared folders, with the folder it is in', async () => {
    const wrapper = await mountPage()
    const backup = rowTexts(wrapper).find(row => row.includes('Hyper Backup (backup data)'))
    expect(backup).toContain('In shared folder share50')
    wrapper.unmount()
  })

  // Explore named a 1.6 TB shared folder as Active Backup for
  // Business data while this page listed only its 271.5 MB database.
  it('lists the Active Backup destination folder Explore names, with the same size and snapshot share', async () => {
    const wrapper = await mountPage('abb_share')
    const row = rowTexts(wrapper).find(row => row.includes('Active Backup for Business (backup data)'))
    expect(row).toContain(formatBytes(1_600_000_000_000))
    expect(row).toContain('In shared folder abb-backup')
    expect(row).toContain('In the package')
    await wrapper.find('button[aria-label="Show details of Active Backup for Business (backup data)"]').trigger('click')
    const detail = wrapper.find('[data-ui-list-detail-pane]').text()
    expect(detail).toContain(formatBytes(494_500_000_000))
    expect(detail).toContain('/volume1/abb-backup')
    wrapper.unmount()
  })

  it('says for each row whether it can be cleared here, in its package, or not deleted', async () => {
    const wrapper = await mountPage()
    const rows = rowTexts(wrapper)
    expect(rows.find(row => row.includes('Container Manager'))).toContain('In the package')
    expect(rows.find(row => row.includes('Installed packages'))).toContain("Can't delete")
    expect(wrapper.text()).not.toMatch(/Can free|\*/)
    wrapper.unmount()
  })

  it('names the exact place and action for a package', async () => {
    const wrapper = await mountPage()
    await wrapper.find('button[aria-label="Show details of Container Manager"]').trigger('click')
    const detail = wrapper.find('[data-ui-list-detail-pane]').text()
    expect(detail).toContain('Container Manager')
    expect(detail).toContain('Image')
    wrapper.unmount()
  })

  // "Hyper Backup（映像與…" and "@img_bkp_cache, @img_bkp_mount"
  // ended in "…" at a 1440-wide DSM window; they wrap instead, so they read in full.
  it('wraps a package and the names it keeps instead of cutting them off', async () => {
    const wrapper = await mountPage()
    const names = wrapper.findAll('tbody tr td:first-child [data-ui-cell-primary]')
    expect(names.length).toBeGreaterThan(0)
    for (const line of names) expect(line.element.closest('[data-layout]')?.hasAttribute('data-wrap')).toBe(true)
    wrapper.unmount()
  })

  it('names a package the catalog does not know by what the CLI calls it', async () => {
    const wrapper = await mountPage()
    expect(rowTexts(wrapper).some(row => row.includes('Other DSM or package data'))).toBe(true)
    wrapper.unmount()
  })

  it('leaves out the Recycle Bins, which have their own page', async () => {
    const wrapper = await mountPage('normal')
    expect(rowTexts(wrapper).join(' ')).not.toContain('Recycle Bin')
    expect(rowTexts(wrapper).length).toBeGreaterThan(0)
    wrapper.unmount()
  })

  it('shows in the detail what the package could free, and that this app never deletes it', async () => {
    const wrapper = await mountPage()
    await wrapper.find('button[aria-label="Show details of Container Manager"]').trigger('click')
    const detail = wrapper.find('[data-ui-list-detail-pane]').text()
    expect(detail).toMatch(/4\.\d+ GB/)
    expect(detail).toContain('Syno Reclaim never deletes package data')
    wrapper.unmount()
  })

  it('says in the detail when a layout is not confirmed and when snapshots were not read', async () => {
    const wrapper = await mountPage()
    expect(wrapper.find('[data-ui-list-detail-list]').text()).not.toContain('Reading not confirmed')
    await wrapper.find('button[aria-label="Show details of Other DSM or package data"]').trigger('click')
    expect(wrapper.find('[data-ui-list-detail-pane]').text()).toContain('best reading')
    await wrapper.find('button[aria-label="Show details of Shared folder snapshots"]').trigger('click')
    expect(wrapper.find('[data-ui-list-detail-pane]').text()).toContain('5 snapshots inside this data were not read')
    wrapper.unmount()
  })

  it('shows the DSM place that manages the package, localized from its id', async () => {
    const wrapper = await mountPage()
    await wrapper.find('button[aria-label="Show details of Installed packages"]').trigger('click')
    expect(wrapper.find('[data-ui-list-detail-pane]').text()).toContain('Package Center')
    wrapper.unmount()
  })

  it('never offers selection or cleanup for a service row', async () => {
    const wrapper = await mountPage()
    expect(wrapper.find('input[type="checkbox"]').exists()).toBe(false)
    expect(wrapper.text()).not.toMatch(/cleanup list|Review/i)
    wrapper.unmount()
  })

  it('expands a row to its paths, the fixed explanation and a link to Explore', async () => {
    const wrapper = await mountPage()
    await wrapper.find('button[aria-label="Show details of iSCSI LUNs"]').trigger('click')
    await settle()
    const detail = wrapper.find('[data-ui-list-detail-pane]')
    expect(detail.text()).toContain('/volume1/share36')
    expect(detail.text()).toContain('UNMAP')
    expect(detail.text()).toContain('Virtual Machine Manager')
    wrapper.unmount()
  })

  it('shows the path count of a summed line', async () => {
    const wrapper = await mountPage()
    await wrapper.find('button[aria-label="Show details of Other DSM or package data"]').trigger('click')
    await settle()
    const detail = wrapper.find('[data-ui-list-detail-pane]')
    expect(detail.text()).toContain('42')
    expect(detail.findAll('.paths li').length).toBeLessThanOrEqual(20)
    wrapper.unmount()
  })

  it('opens a path of the row in Explore', async () => {
    const wrapper = await mountPage()
    await wrapper.find('button[aria-label="Show details of iSCSI LUNs"]').trigger('click')
    await settle()
    await wrapper.find('button[aria-label="Open /volume1/share36 in Explore"]').trigger('click')
    expect(nav.page).toBe('explore')
    expect(takeExploreRequest()).toEqual({ path: '/volume1/share36' })
    wrapper.unmount()
  })

  it('collapses the detail when the same row is toggled again', async () => {
    const wrapper = await mountPage()
    await wrapper.find('button[aria-label="Show details of Container Manager"]').trigger('click')
    expect(wrapper.find('[data-ui-list-detail-pane]').exists()).toBe(true)
    await wrapper.find('button[aria-label="Hide details of Container Manager"]').trigger('click')
    expect(wrapper.find('[data-ui-list-detail-pane]').exists()).toBe(false)
    wrapper.unmount()
  })

  it('shows the selected package detail to the right of the list, keeping the row selected', async () => {
    // A real Transition is DOM-transparent; its test stub adds a wrapper.
    const wrapper = await mountPage('packages', true)
    expect(wrapper.find('[data-ui-list-detail]').exists()).toBe(true)
    // No selection: the list stands alone.
    expect(wrapper.find('[data-ui-list-detail-pane]').exists()).toBe(false)

    await wrapper.find('button[aria-label="Show details of iSCSI LUNs"]').trigger('click')
    await settle()

    const list = wrapper.find('[data-ui-list-detail-list]')
    const detail = wrapper.find('[data-ui-list-detail-pane]')
    const separator = wrapper.find('[data-ui-list-detail-separator]')
    // One row container holds list then detail: the detail is beside, not below.
    expect(detail.element.parentElement).toBe(list.element.parentElement)
    expect([...list.element.parentElement!.children]).toEqual([list.element, separator.element, detail.element])
    expect(detail.text()).toContain('/volume1/share36')

    // Selection context stays on the list row.
    const row = wrapper.findAll('tbody tr').find(candidate => candidate.text().includes('iSCSI LUNs'))!
    expect(row.find('button[aria-expanded="true"]').exists()).toBe(true)
    wrapper.unmount()
  })

  it('expands the package an Explore service row asked for', async () => {
    openPackage('iscsi_luns', '/volume1/share36')
    const wrapper = await mountPage()
    expect(wrapper.find('[data-ui-list-detail-pane]').text()).toContain('/volume1/share36')
    expect(nav.packageRequest).toBeNull()
    wrapper.unmount()
  })

  it('asks for a scan when the volume has no report', async () => {
    const wrapper = await mountPage('no_report')
    expect(wrapper.text()).toContain('Not scanned yet')
    expect(wrapper.find('tbody').exists()).toBe(false)
    wrapper.unmount()
  })

  it('shows an error state for a failed answer', async () => {
    const wrapper = await mountPage('error')
    expect(wrapper.text()).toContain('Something went wrong')
    wrapper.unmount()
  })

  it('says so when the volume has no package data', async () => {
    const base = fixtureBackend()
    setBackend(async (method, params) => {
      const answer = await base(method, params)
      if (method !== 'top.list') return answer
      const empty = structuredClone(answer) as TopDocument
      empty.results[0].services.lines = []
      return empty
    })
    volumes.selected = VOLUME
    await loadVolumes()
    const wrapper = mount(PackagesPage, { attachTo: document.body })
    await settle()
    expect(wrapper.text()).toContain('No package data')
    wrapper.unmount()
  })

  it('reads the answer again after a cleanup invalidates it', async () => {
    const base = fixtureBackend({ scenario: 'packages' })
    let calls = 0
    setBackend(async (method, params) => {
      if (method === 'top.list') calls += 1
      return base(method, params)
    })
    volumes.selected = VOLUME
    await loadVolumes()
    const wrapper = mount(PackagesPage, { attachTo: document.body })
    await settle()
    const before = calls
    invalidateAnswers()
    await settle()
    expect(calls).toBeGreaterThan(before)
    wrapper.unmount()
  })
})
