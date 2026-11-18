import { enableAutoUnmount, flushPromises, mount, type VueWrapper } from '@vue/test-utils'
import { Callout, formatDateTime } from '@synopackageland/ui'
import { setBackend } from '@/api'
import type { Backend, ResultOf } from '@/api/methods'
import { fixtureBackend } from '@/api/fixtures'
import { useBasket } from '@/stores/basket'
import { jobs, refreshJobs } from '@/stores/jobs'
import { loadVolumes, volumes } from '@/stores/volumes'
import { PAGE_ACTIONS, showPageActionsInPlace } from '@/shell/pageActionsInPlace'
import { pageStatusText, pageStatusTone } from '@/components/pageInfoTesting'
import RecyclePage from './RecyclePage.vue'
import { nav } from '@/stores/nav'

const VOLUME = '/volume1'

showPageActionsInPlace()

/** Mounts the page over a fixture scenario, or over a backend a test wraps. */
// Dispose watchers even if setup or an assertion fails.
enableAutoUnmount(afterEach)

async function mountPage(scenario: string | Backend = 'interaction', realTransitions = false): Promise<VueWrapper> {
  setBackend(typeof scenario === 'string' ? fixtureBackend({ scenario }) : scenario)
  volumes.selected = VOLUME
  await loadVolumes()
  const wrapper = mount(RecyclePage, { attachTo: document.body, global: { stubs: { transition: !realTransitions } } })
  await flushPromises()
  await vi.dynamicImportSettled()
  await flushPromises()
  return wrapper
}

async function expandShare(wrapper: VueWrapper, share: string) {
  const show = wrapper.find(`button[aria-label="Show oldest items in ${share}"]`)
  await show.trigger('click')
  await flushPromises()
  await vi.dynamicImportSettled()
  await flushPromises()
}

async function expandShare01(wrapper: VueWrapper) {
  await expandShare(wrapper, 'share01')
}

function button(wrapper: VueWrapper, label: string) {
  return wrapper.findAll('button').find(b => b.text() === label)!
}

/** A DSM desktop stub that records File Station launches. */
function stubFileStation() {
  const calls: { id: string; params?: { path: string } }[] = []
  ;(window as unknown as { SYNO?: unknown }).SYNO = {
    SDS: { AppLaunch: (id: string, params?: { path: string }) => calls.push({ id, params }) },
  }
  return calls
}

// The fixtures' newest items were moved on 2026-09-20; "older than 30 days" must not depend on today.
const NOW = Date.parse('2026-10-01T00:00:00Z')

beforeEach(() => {
  vi.spyOn(Date, 'now').mockReturnValue(NOW)
  sessionStorage.clear()
  localStorage.clear()
  jobs.list = []
  useBasket(VOLUME).clear()
  document.body.innerHTML = ''
  delete (window as unknown as { SYNO?: unknown }).SYNO
})

afterEach(() => {
  vi.restoreAllMocks()
})

describe('Recycle Bin page', () => {
  it('explains a deleted folder, offers a new scan and copies the actual resolution error', async () => {
    const backend = fixtureBackend({ scenario: 'interaction' })
    const cause = { code: 'not_found', path: '/volume1/share01', message: 'No such file or directory (os error 2)' }
    const writeText = vi.fn().mockResolvedValue(undefined)
    vi.stubGlobal('navigator', { clipboard: { writeText } })
    nav.newScanOpen = false
    try {
      const wrapper = await mountPage((async (method, params) => {
        const answer = await backend(method, params)
        if (method !== 'recycle.list' || !(params as { oldest?: number }).oldest) return answer
        return { ...answer, completion: 'failed', errors: [cause], report_info: null, shares: [], exit: 4 }
      }) as Backend)
      await expandShare01(wrapper)
      const pane = wrapper.get('[data-testid="recycle-detail"]')
      expect(pane.text()).toContain('Folder not found')
      expect(pane.text()).toContain('Scan the whole volume again')
      expect(pane.text()).not.toContain('This Recycle Bin is empty')
      expect(pane.text()).not.toContain('Add to cleanup list')
      await button(wrapper, 'Copy details').trigger('click')
      await flushPromises()
      expect(JSON.parse(writeText.mock.calls[0]![0])).toMatchObject({ code: 'not_found', exit: 4, errors: [cause] })
      await button(wrapper, 'Scan now').trigger('click')
      expect(nav.newScanOpen).toBe(true)
    } finally {
      nav.newScanOpen = false
      vi.unstubAllGlobals()
    }
  })

  it('queries the homes shared-folder root and selects the requested user bin, including load more', async () => {
    const backend = fixtureBackend({ scenario: 'interaction' })
    const seen: Record<string, unknown>[] = []
    const wrapper = await mountPage((async (method, params) => {
      if (method !== 'recycle.list') return backend(method, params)
      seen.push(params as Record<string, unknown>)
      const answer = await backend('recycle.list', { target: '/volume1/share01', oldest: 200 })
      const template = answer.shares[0]!
      const bin = (user: string) => ({
        ...template, share: { path: `homes/${user}` }, items: 500,
        oldest: [{ ...template.oldest![0]!, path: { path: `/volume1/homes/${user}/#recycle/${user}.bin` } }],
      })
      return { ...answer, shares: [bin('alice'), bin('bob')] }
    }) as Backend)
    await expandShare(wrapper, 'homes/bob')
    expect(seen.at(-1)).toEqual({ target: '/volume1/homes', oldest: 200 })
    expect(wrapper.get('[data-testid="recycle-detail"]').text()).toContain('bob.bin')
    expect(wrapper.get('[data-testid="recycle-detail"]').text()).not.toContain('alice.bin')
    await button(wrapper, 'Load more').trigger('click')
    await flushPromises()
    expect(seen.at(-1)).toEqual({ target: '/volume1/homes', oldest: 1000 })
  })

  it('remembers the keyboard-adjusted detail width and closes the selected share from the pane', async () => {
    const wrapper = await mountPage()
    await expandShare01(wrapper)
    const separator = wrapper.get('[data-ui-list-detail-separator]')
    const initialWidth = Number(separator.attributes('aria-valuenow'))
    await separator.trigger('keydown', { key: 'ArrowLeft' })
    expect(Number(separator.attributes('aria-valuenow'))).toBe(initialWidth + 16)
    const key = Object.keys(localStorage).find(key => key.startsWith('syno-pkg-land-ui:detail-width:') && key.endsWith(':recycle'))!
    expect(localStorage.getItem(key)).toBe(String(initialWidth + 16))
    wrapper.unmount()

    const restored = await mountPage()
    await expandShare01(restored)
    expect(Number(restored.get('[data-ui-list-detail-separator]').attributes('aria-valuenow'))).toBe(initialWidth + 16)
    await restored.get('[data-ui-list-detail-close]').trigger('click')
    expect(restored.find('[data-ui-list-detail-pane]').exists()).toBe(false)
    restored.unmount()
  })

  it('keeps moved dates compact and unwrapped in a 140px column', async () => {
    const wrapper = await mountPage()
    await expandShare01(wrapper)
    const detail = wrapper.get('[data-ui-list-detail-pane]')
    const row = detail.findAll('tbody tr').find(row => row.find('[data-ui-tooltip-text="/volume1/share01/#recycle/file0001.bin"]').exists())!
    const recorded = await fixtureBackend({ scenario: 'interaction' })('recycle.list', { target: '/volume1/share01', oldest: 200 })
    const item = recorded.shares[0]!.oldest!.find(item => item.path.path.endsWith('/file0001.bin'))!
    const cell = row.get('[data-column="moved"]')
    expect(cell.text()).toBe(formatDateTime(item.ctime, { locale: 'en', style: 'compact', now: NOW }))
    expect(cell.find('[data-wrap]').exists()).toBe(false)
    const columnIndex = (detail.get('th[data-column="moved"]').element as HTMLTableCellElement).cellIndex
    expect(detail.findAll('col')[columnIndex]!.attributes('style')).toContain('140px')
    wrapper.unmount()
  })

  it('shows the Recycle Bins of shares scanned on their own when the volume has no report', async () => {
    const wrapper = await mountPage('share_scan')
    expect(wrapper.text()).not.toContain('Not scanned yet')
    expect(wrapper.text()).toContain('No whole-volume scan yet')
    expect(wrapper.findAll('tbody tr').map(row => row.text())[0]).toContain('share04')
    await expandShare(wrapper, 'share04')
    expect(wrapper.find('[data-ui-tooltip-text="/volume1/share04/#recycle/file0003.bin"]').text()).toBe('file0003.bin')
    wrapper.unmount()
  })

  it('prefers a newer share report while retaining bins from an older volume report', async () => {
    const volume = fixtureBackend({ scenario: 'interaction' })
    const shared = fixtureBackend({ scenario: 'share_scan' })
    const wrapper = await mountPage((async (method, params) => {
      if (method === 'history.list') {
        const history = await shared('history.list', {})
        return { ...history, scans: history.scans.map(scan => ({ ...scan, finished: '2030-01-01T00:00:00Z', targets: [{ path: '/volume1/new-share' }] })) }
      }
      if (method === 'recycle.list' && (params as { target: string }).target === '/volume1/new-share') {
        const result = await shared('recycle.list', { target: '/volume1/share04' })
        return { ...result, report_info: { ...result.report_info!, scan_finished: '2030-01-01T00:00:00Z' }, shares: [{ ...result.shares[0]!, share: { path: 'new-share' }, items: 17 }] }
      }
      return volume(method, params)
    }) as Backend)
    expect(wrapper.text()).toContain('new-share')
    expect(wrapper.text()).toContain('share01')
    expect(wrapper.text()).not.toContain('No whole-volume scan yet')
    wrapper.unmount()
  })

  describe('which report answers each share', () => {
    /** The CLI's volume answer with share04 listed from its newer share report (SPEC 5.16). */
    function newerShare04(seen: Record<string, unknown>[] = []): Backend {
      const volume = fixtureBackend({ scenario: 'interaction' })
      const shared = fixtureBackend({ scenario: 'share_scan' })
      return (async (method, params) => {
        if (method === 'recycle.list') seen.push(params as Record<string, unknown>)
        const target = (params as { target: string; report?: string }).target
        if (method === 'recycle.list' && target === '/volume1' && !(params as { report?: string }).report) {
          const answer = await volume(method, params) as ResultOf<'recycle.list'>
          const share = await shared('recycle.list', { target: '/volume1/share04' })
          return { ...answer, shares: [...answer.shares, { ...share.shares[0]!, report_info: share.report_info! }] }
        }
        if (method === 'recycle.list' && target === '/volume1/share04') return shared(method, params)
        return volume(method, params)
      }) as Backend
    }
    const rowOf = (wrapper: VueWrapper, share: string) => wrapper.findAll('tbody tr').find(row => row.text().includes(share))

    it('marks a share its newer share report answers, and names the mix in the chip', async () => {
      const wrapper = await mountPage(newerShare04())
      expect(rowOf(wrapper, 'share04')?.text()).toContain('Shared folder scan')
      expect(rowOf(wrapper, 'share01')?.text()).not.toContain('Shared folder scan')
      expect(pageStatusText()).toContain('Whole volume + 1 newer shared folder')
      wrapper.unmount()
    })

    it('answers from the whole-volume report alone once the user picks it, the detail too', async () => {
      const seen: Record<string, unknown>[] = []
      const wrapper = await mountPage(newerShare04(seen))
      const picker = wrapper.findComponent({ name: 'ReportPicker' })
      expect(picker.exists()).toBe(true)
      picker.vm.$emit('update:modelValue', 'fixture-job0010')
      await flushPromises()
      await vi.dynamicImportSettled()
      await flushPromises()

      expect(seen).toContainEqual({ target: '/volume1', report: 'fixture-job0010' })
      expect(rowOf(wrapper, 'share04')).toBeUndefined()
      expect(pageStatusText()).not.toContain('newer shared folder')
      await expandShare01(wrapper)
      expect(seen.at(-1)).toEqual({ target: '/volume1/share01', oldest: 200, report: 'fixture-job0010' })
      wrapper.unmount()
    })
  })

  // B22: the acceptance suite finds the open share's pane by this id; the SDK ListDetail (706e60f4) dropped it.
  it('names the open share in the detail pane the suite finds by its test id, and removes the pane on close', async () => {
    const wrapper = await mountPage()
    await expandShare01(wrapper)
    const pane = document.querySelector('[data-testid="recycle-detail"]')
    expect(pane?.querySelector('h2')?.textContent).toBe('share01')
    expect(pane?.querySelector('h3')?.textContent).toBe('Oldest items')
    await wrapper.find('button[aria-label="Hide oldest items in share01"]').trigger('click')
    await flushPromises()
    expect(document.querySelector('[data-testid="recycle-detail"]')).toBeNull()
    wrapper.unmount()
  })

  it('loads share answers one page at a time instead of one call per share', async () => {
    const base = fixtureBackend({ scenario: 'share_scan' })
    const wrapper = await mountPage(async (method, params) => {
      const answer = await base(method, params)
      if (method !== 'history.list') return answer
      const doc = structuredClone(answer) as ResultOf<'history.list'>
      const template = doc.scans.find(scan => !scan.volume && scan.report != null && scan.outcome === 'completed')!
      doc.scans = Array.from({ length: 60 }, (_, index) => ({
        ...structuredClone(template),
        targets: [{ path: `/volume1/share${String(index).padStart(2, '0')}` }],
      }))
      return doc
    })

    // 60 shares, one page of 50: the other 10 wait for the user.
    expect(wrapper.text()).toContain('Load 10 more shared folders')
    wrapper.unmount()
  })

  it('says so when no share scanned on its own has a Recycle Bin', async () => {
    const recorded = fixtureBackend({ scenario: 'share_scan' })
    const wrapper = await mountPage(async (method, params) => {
      const answer = await recorded(method, params)
      if (method !== 'recycle.list' || (params as { target: string }).target === VOLUME) return answer
      return { ...answer, completion: 'failed', errors: [{ code: 'no_recycle_bin', message: 'none' }], report_info: null, shares: [] } as never
    })
    expect(wrapper.text()).not.toContain('Not scanned yet')
    expect(wrapper.text()).toContain('No whole-volume scan yet')
    expect(wrapper.text()).toContain('No Recycle Bins in the scanned shared folders')
    wrapper.unmount()
  })

  it('keeps the recorded share-level recycle response addressable', async () => {
    const recorded = await fixtureBackend()('recycle.list', { target: '/volume1/share73', oldest: 200 })
    expect(recorded.target.path).toBe('/volume1/share73')
    expect(recorded.shares[0]?.share.path).toBe('/volume1/share73')
  })

  it('shows the live share aggregate from the captured volume report', async () => {
    const wrapper = await mountPage('normal')
    expect(wrapper.text()).toContain('share73')
    expect(wrapper.text()).toContain('87')
    wrapper.unmount()
  })

  it('opens a shared folder from its name link, making a relative share path a virtual folder path', async () => {
    const calls = stubFileStation()
    const wrapper = await mountPage('interaction')
    // The interaction volumes.list fixture lists share04 only; DSM lists every share with a Recycle Bin.
    volumes.document!.volumes[0].shares.push({ name: { path: 'share01' }, path: { path: '/volume1/share01' }, subvolume_id: 300 })
    await expandShare01(wrapper)
    expect(wrapper.get('[data-ui-list-detail-pane]').attributes()).not.toHaveProperty('data-ui-surface')
    await wrapper.find('[data-ui-list-detail-pane] button[aria-label="Open share01 in File Station"]').trigger('click')
    expect(calls).toEqual([{ id: 'SYNO.SDS.App.FileStation3.Instance', params: { opendir: '/share01' } }])
    wrapper.unmount()
  })

  // A narrow list must not break "/volume1/share50" across lines: the name
  // under the volume is enough there, on one line, with the full path for the UI tooltip.
  it('names each shared folder under its volume on one line, with the full path for the UI tooltip', async () => {
    const wrapper = await mountPage('normal')
    const cell = wrapper.get('[data-ui-list-detail-list] [data-ui-tooltip-text="/volume1/share59/folder0149"]')
    expect(cell.text()).toBe('share59/folder0149')
    expect(wrapper.get('[data-ui-list-detail-list]').text()).not.toContain('/volume1/')
    expect(wrapper.find('[data-ui-list-detail-list] [data-wrap]').exists()).toBe(false)
    wrapper.unmount()
  })

  it('gives the selected share an accessible pane and vertical resize separator', async () => {
    const wrapper = await mountPage('normal')
    await expandShare(wrapper, '/volume1/share50')
    const detail = wrapper.get('[data-ui-list-detail-pane]')
    const separator = wrapper.get('[data-ui-list-detail-separator]')
    expect(detail.attributes('aria-label')).toBe('share50')
    expect(separator.attributes('aria-controls')).toBe(detail.attributes('id'))
    expect(separator.attributes('aria-orientation')).toBe('vertical')
    wrapper.unmount()
  })

  it('opens a nested shared folder from its absolute path', async () => {
    const calls = stubFileStation()
    const wrapper = await mountPage('normal')
    const share = '/volume1/share59/folder0149'
    await expandShare(wrapper, share)
    await wrapper.find(`[data-ui-list-detail-pane] button[aria-label="Open ${share} in File Station"]`).trigger('click')
    expect(calls).toEqual([{ id: 'SYNO.SDS.App.FileStation3.Instance', params: { opendir: '/share59/folder0149' } }])
    wrapper.unmount()
  })

  it("lists each shared folder's Recycle Bin with size and can free, and its holders in the detail", async () => {
    const wrapper = await mountPage()
    const text = wrapper.text()
    expect(text).toContain('share01')
    expect(text).toContain('Allocated')
    expect(text).toContain('188 GB')
    expect(text).toContain('186 GB')
    expect(text).not.toContain('hard link 2 GB')
    const row = wrapper.findAll('tbody tr').find(item => item.text().includes('share01'))!
    expect(row.find('button[aria-label="Show oldest items in share01"]').text()).toBe('share01')
    await expandShare01(wrapper)
    const detail = wrapper.find('[data-ui-list-detail-pane]').text()
    expect(detail).toContain('hard link 2 GB')
    wrapper.unmount()
  })

  it("shows a share's detail beside the list when its row is selected, and closes it on a second click", async () => {
    // Check the real DOM shape rather than TransitionStub's extra element.
    const wrapper = await mountPage('interaction', true)
    expect(wrapper.find('[data-ui-list-detail-pane]').exists()).toBe(false)
    await expandShare01(wrapper)
    const list = wrapper.find('[data-ui-list-detail-list]')
    const detail = wrapper.find('[data-ui-list-detail-pane]')
    const separator = wrapper.find('[data-ui-list-detail-separator]')
    expect([...list.element.parentElement!.children]).toEqual([list.element, separator.element, detail.element])
    expect(detail.text()).toContain('share01')
    const row = wrapper.findAll('tbody tr').find(item => item.text().includes('share01'))!
    expect(row.find('button[aria-expanded="true"]').exists()).toBe(true)
    await wrapper.find('button[aria-label="Hide oldest items in share01"]').trigger('click')
    expect(wrapper.find('[data-ui-list-detail-pane]').exists()).toBe(false)
    wrapper.unmount()
  })

  it('leaves the holder cell empty for a share nothing else holds', async () => {
    const base = fixtureBackend({ scenario: 'interaction' })
    const wrapper = await mountPage(async (method, params) => {
      const answer = await base(method, params)
      if (method !== 'recycle.list') return answer
      const doc = structuredClone(answer) as ResultOf<'recycle.list'>
      for (const share of doc.shares) share.held_by = []
      return doc
    })
    const row = wrapper.findAll('tbody tr').find(item => item.text().includes('share01'))!
    expect(row.text()).not.toContain('[]')
    wrapper.unmount()
  })

  it('shows the oldest items, the estimated-moved header and how many of the total are loaded', async () => {
    const wrapper = await mountPage()
    await expandShare01(wrapper)
    const text = wrapper.text()
    expect(wrapper.find('[data-ui-list-detail-pane] h2').text()).toBe('share01')
    expect(text).toContain('Oldest items')
    expect(text).toContain('Moved (est.)')
    expect(text).toContain('Showing the first 200 of 250 items')
    expect(text.match(/188 GB/g)).toHaveLength(1)
    expect(wrapper.find('[data-ui-tooltip-text="/volume1/share01/#recycle/file0001.bin"]').exists()).toBe(true)
    expect(wrapper.find('[aria-label="Hide oldest items in share01"]').exists()).toBe(true)
    expect(wrapper.find('[aria-label="Empty items older than"]').exists()).toBe(false)
    wrapper.unmount()
  })

  it('offers nothing to select: Syno Reclaim never removes anything from a Recycle Bin', async () => {
    const wrapper = await mountPage()
    await expandShare01(wrapper)
    expect(wrapper.find('[aria-label="Select row /volume1/share01/#recycle/file0001.bin"]').exists()).toBe(false)
    expect(wrapper.findAll('button').some(b => b.text() === 'Add to cleanup list')).toBe(false)
    expect(wrapper.get('[data-test="recycle-dsm"]').text()).toContain('never empties a Recycle Bin')
    expect(wrapper.get('[data-test="recycle-dsm"]').text()).toContain('Empty Recycle Bin')
    expect(useBasket(VOLUME).count.value).toBe(0)
    wrapper.unmount()
  })

  it('shows what Syno Reclaim moved since the scan, not yet in the sizes', async () => {
    const backend = fixtureBackend({ scenario: 'interaction' })
    setBackend(async (method, params) => {
      const doc = await backend(method, params)
      if (method === 'recycle.list') {
        const recycle = doc as ResultOf<'recycle.list'>
        recycle.moved_by_reclaim = recycle.shares.slice(0, 1).map(share => ({ share: share.share, items: 3, bytes: 2 * 1024 * 1024 * 1024, last_moved: '2026-10-01T00:00:00Z' }))
      }
      return doc
    })
    volumes.selected = VOLUME
    await loadVolumes()
    const wrapper = mount(RecyclePage, { attachTo: document.body })
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    expect(wrapper.get('[data-test="recycle-moved"]').text()).toContain('Moved here by Syno Reclaim since the scan: 2 GB in 1 shared folder, not in the sizes below yet')
    await expandShare01(wrapper)
    expect(wrapper.get('[data-test="recycle-share-moved"]').text()).toContain('3 items')
    wrapper.unmount()
  })

  it('loads the remaining oldest items up to the CLI maximum', async () => {
    const wrapper = await mountPage()
    await expandShare01(wrapper)
    await button(wrapper, 'Load more').trigger('click')
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    expect(wrapper.text()).not.toContain('Showing the first 200 of 250 items')
    expect(wrapper.text()).toContain('unknown')
    wrapper.unmount()
  })

  it('shows what a volume without cleanup could reclaim but offers nothing to select', async () => {
    const backend = fixtureBackend({ scenario: 'interaction' })
    setBackend(async (method, params) => {
      const doc = await backend(method, params)
      if (method === 'volumes.list') {
        for (const volume of (doc as ResultOf<'volumes.list'>).volumes)
          volume.capabilities = { ...volume.capabilities, cleanup: false, cleanup_unavailable_reason: 'rename_noreplace_unavailable' }
      }
      return doc
    })
    volumes.selected = VOLUME
    await loadVolumes()
    useBasket(VOLUME).add({ path: '/volume1/share01/#recycle/file0001.bin', reclaimable: 1 })
    const wrapper = mount(RecyclePage, { attachTo: document.body })
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    await expandShare01(wrapper)
    const text = wrapper.text()
    expect(text).not.toContain('Can free')
    expect(text.match(/Reclaimable \(this app cannot clean it up\)/g)?.length).toBe(2)
    expect(wrapper.find('[aria-label="Select row /volume1/share01/#recycle/file0001.bin"]').exists()).toBe(false)
    expect(wrapper.findAll('button').some(b => b.text() === 'Add to cleanup list')).toBe(false)
    expect(text).not.toContain('can be freed')
    wrapper.unmount()
  })

  it('shows a muted note, not the banner, for attribution-only unknown above the threshold; the banner stays for a coverage gap', async () => {
    const large = await mountPage('unknown_large')
    expect(large.text()).toContain('could not be attributed and is not counted as reclaimable')
    expect(large.text()).not.toContain('Some data could not be read')
    large.unmount()
    document.body.innerHTML = ''
    const gap = await mountPage('coverage_gap')
    expect(gap.text()).toContain('Some data could not be read')
    expect(gap.text()).not.toContain('could not be attributed and is not counted as reclaimable')
    gap.unmount()
  })

  it('shows one muted line, not the banner or partial chip, for a single entry that changed during the scan; eleven keep the banner', async () => {
    const churn = await mountPage('namespace_churn')
    expect(churn.text()).toContain('1 item changed during the scan and was skipped')
    expect(churn.text()).not.toContain('Some data could not be read')
    expect(document.body.textContent).not.toContain('Not fully read')
    churn.unmount()
    document.body.innerHTML = ''
    const many = await mountPage('namespace_churn_many')
    expect(many.text()).toContain('Some data could not be read')
    expect(many.text()).not.toContain('changed during the scan and was skipped')
    many.unmount()
  })

  // The library's DataGrid grows into any height the page reserves, which drew
  // a blank band between the last row and the item count.
  it('sizes the page to its content, so the share grid ends at its last row', async () => {
    const wrapper = await mountPage()
    expect(getComputedStyle(wrapper.find('.recycle-page').element).minHeight).not.toBe('100%')
    wrapper.unmount()
  })

  // The header badge already says the scan was not fully read; the page adds one quiet line.
  it('shows a partial report as one quiet line, not a warning callout', async () => {
    const wrapper = await mountPage('partial')
    expect(wrapper.findAllComponents(Callout).some(candidate => candidate.props('title') === 'Some data could not be read')).toBe(false)
    const hint = wrapper.get('[data-testid="partial-hint"]')
    expect(hint.text()).toContain('Some data could not be read')
    expect(hint.text()).toContain('The age estimate is unavailable')
    expect(pageStatusTone()).toBe('warning')
    expect(wrapper.find('[data-testid="scan-notes"]').exists()).toBe(false)
    wrapper.unmount()
  })

  it('shows a stale report without a second scan button beside New scan', async () => {
    const wrapper = await mountPage('stale')
    const chip = pageStatusText()
    expect(chip).not.toContain('Rescan')
    expect(chip).toContain('days ago')
    expect(pageStatusTone()).toBe('warning')
    wrapper.unmount()
  })

  it('shows the empty state when a scanned volume has no Recycle Bins', async () => {
    const wrapper = await mountPage('empty')
    expect(wrapper.text()).toContain('No Recycle Bins on this volume')
    expect(wrapper.text()).not.toContain('share01')
    wrapper.unmount()
  })

  it('shows a partial oldest-items answer as the same quiet line', async () => {
    const wrapper = await mountPage()
    const backend = fixtureBackend({ scenario: 'interaction' })
    setBackend(async (method, params) => {
      const answer = await backend(method, params)
      return method === 'recycle.list' && (params as { oldest?: number }).oldest ? ({ ...(answer as object), completion: 'partial' } as never) : answer
    })
    await expandShare01(wrapper)
    const hint = wrapper.get('[data-ui-list-detail-pane] [data-testid="partial-hint"]')
    expect(hint.text()).toContain('Unknown parts are never counted as reclaimable')
    wrapper.unmount()
  })

  it('surfaces a complete report warning returned by the oldest-items request as a note', async () => {
    const wrapper = await mountPage('age_warning')
    await expandShare01(wrapper)
    expect(wrapper.find('[data-ui-list-detail-pane] [data-testid="scan-notes"]').text()).toContain('The age estimate is unavailable')
    wrapper.unmount()
  })

  it('shows changed-during-scan consistency on the report chip', async () => {
    const wrapper = await mountPage('changed')
    expect(pageStatusText()).toContain('Changed during scan')
    wrapper.unmount()
  })

  it('shows the consistency state once in the report chip', async () => {
    const wrapper = await mountPage('changed_system')
    expect(pageStatusText().match(/Changed during scan/g)).toHaveLength(1)
    wrapper.unmount()
  })

  it('words the unresolved-references warning instead of showing the raw code', async () => {
    const wrapper = await mountPage('changed_system')
    expect(wrapper.find('[data-testid="scan-notes"]').text()).toContain('Some data could not be attributed to an owner')
    expect(wrapper.text()).not.toContain('Unrecognised status')
    wrapper.unmount()
  })

  it('shows the consistency state once in the expanded share report', async () => {
    const backend = fixtureBackend({ scenario: 'changed_system' })
    const wrapper = await mountPage(async (method, params) => {
      const answer = await backend(method, params)
      if (method !== 'recycle.list' || (params as { target?: string }).target === VOLUME) return answer
      const document = answer as { report_info: object }
      return { ...document, report_info: { ...document.report_info, consistency: 'changed-during-scan' } } as never
    })
    await expandShare(wrapper, '/volume1/share50')
    expect(wrapper.find('.recycle-detail-meta').text().match(/Changed during scan/g)).toHaveLength(1)
    wrapper.unmount()
  })

  it('reassures when only system areas changed during the scan', async () => {
    const wrapper = await mountPage('changed_system')
    expect(pageStatusText()).toContain('Changed during scan')
    expect(wrapper.find('[data-testid="scan-notes"]').text()).toContain(
      "Changes were only in system areas (DSM's own writes); cleanup in shared folders is not affected",
    )
    wrapper.unmount()
  })

  it('has no system-areas note when a shared folder changed', async () => {
    const wrapper = await mountPage('changed_share')
    expect(wrapper.text()).not.toContain('Changes were only in system areas')
    wrapper.unmount()
  })

  it('offers a scan when the volume has no report', async () => {
    const wrapper = await mountPage('no_report')
    expect(wrapper.text()).toContain('Not scanned yet')
    expect(wrapper.text()).toContain('Scan now')
    wrapper.unmount()
  })

  it.each([true, false])('reports copy success accurately when DSM blocks the Clipboard API (fallback: %s)', async copied => {
    const writeText = vi.fn().mockRejectedValue(new DOMException('Blocked in frame', 'NotAllowedError'))
    const execCommand = vi.fn().mockReturnValue(copied)
    const clipboardDescriptor = Object.getOwnPropertyDescriptor(navigator, 'clipboard')
    const commandDescriptor = Object.getOwnPropertyDescriptor(document, 'execCommand')
    Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText } })
    Object.defineProperty(document, 'execCommand', { configurable: true, value: execCommand })
    const wrapper = await mountPage('error')
    try {
      await button(wrapper, 'Copy details').trigger('click')
      await flushPromises()
      expect(execCommand).toHaveBeenCalledWith('copy')
      expect(wrapper.text()).toContain(copied ? 'Details copied' : 'Could not copy')
    } finally {
      wrapper.unmount()
      if (clipboardDescriptor) Object.defineProperty(navigator, 'clipboard', clipboardDescriptor)
      else Reflect.deleteProperty(navigator, 'clipboard')
      if (commandDescriptor) Object.defineProperty(document, 'execCommand', commandDescriptor)
      else Reflect.deleteProperty(document, 'execCommand')
    }
  })

  it('shows a localized error when the command fails', async () => {
    const wrapper = await mountPage('error')
    expect(wrapper.text()).toContain('Something went wrong')
    expect(wrapper.text()).toContain('The Syno Reclaim command failed')
    wrapper.unmount()
  })

  it('shows the paused job chip while this volume has a paused scan', async () => {
    const wrapper = await mountPage('paused')
    await refreshJobs()
    await flushPromises()
    expect(pageStatusText()).toContain('Paused at 35%')
    expect(pageStatusTone()).toBe('warning')
    expect(document.querySelector(PAGE_ACTIONS)!.textContent).toContain('View')
    wrapper.unmount()
  })

  it('falls back to entry progress when the saved item-count basis is unavailable', async () => {
    const wrapper = await mountPage('paused_entries')
    await refreshJobs()
    await flushPromises()
    const chip = pageStatusText()
    expect(chip).toContain('Paused at 23%')
    wrapper.unmount()
  })

  // The job strip shows the running scan; the page actions keep the data time, as on the other pages.
  it('keeps the report chip while this volume has an active scan', async () => {
    const wrapper = await mountPage('scanning')
    await refreshJobs()
    await flushPromises()
    expect(pageStatusText()).not.toContain('scanning…')
    expect(pageStatusText()).toContain(' ago')
    wrapper.unmount()
  })

  it('shows a returned CLI refusal code instead of calling a Recycle Bin empty', async () => {
    const wrapper = await mountPage('detail_refusal')
    await expandShare01(wrapper)
    expect(wrapper.text()).toContain('No Recycle Bin')
    expect(wrapper.text()).not.toContain('This Recycle Bin is empty')
    expect(wrapper.text()).not.toContain('Add to cleanup list')
    expect(wrapper.text()).not.toContain('Empty items older than')
    wrapper.unmount()
  })

  it('shows the busy state while another writer holds the volume', async () => {
    const wrapper = await mountPage('busy')
    expect(wrapper.text()).toContain('Another scan is using this volume')
    wrapper.unmount()
  })
})
