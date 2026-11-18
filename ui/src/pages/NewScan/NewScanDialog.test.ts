import { flushPromises, mount } from '@vue/test-utils'
import type { Backend, Method } from '@/api/methods'
import { ApiError, setBackend } from '@/api'
import { apiErrorFromCgi } from '@/api/errors'
import { fixtureBackend } from '@/api/fixtures'
import App from '@/App.vue'
import { allowAuthorizationForTests, declineRootWork } from '@/authorizationFixture'
import { jobs } from '@/stores/jobs'
import { nav } from '@/stores/nav'
import { volumes } from '@/stores/volumes'
import { formatFileSize } from '@synopackageland/ui'
import capturedVolumes from '../../../fixtures/volumes.list/volume1.json'
import capturedOverview from '../../../fixtures/overview.get/volume1.json'

// Mounting App.vue needs the startup gate ready; the gate itself is tested separately.
allowAuthorizationForTests()

// Date-only fixture clocks must not leak into later tests.
afterEach(() => vi.useRealTimers())

/** The radio button of the SDK RadioGroup option whose label starts with `text`. */
const radio = (text: string) => [...document.body.querySelectorAll('label')]
  .find(label => label.querySelector('[role="radio"]') && label.textContent?.trim().startsWith(text))
  ?.querySelector<HTMLButtonElement>('[role="radio"]')

/** Wait until New scan is open with its volumes and history loaded: the setup mask is gone. */
const setupLoaded = () => vi.waitFor(() => {
  const dialog = document.body.querySelector('[role="dialog"]')
  expect(dialog?.textContent).toContain('Choose a volume')
  expect(dialog!.querySelector('[aria-busy="true"]')).toBeNull()
}, { timeout: 5000 })

describe('NewScanDialog', () => {
  it('describes an ext4 rescan without promising snapshots or cache reuse', async () => {
    const recorded = fixtureBackend({ scenario: 'interaction' })
    setBackend(async (method, params) => {
      const answer = await recorded(method, params)
      if (method !== 'volumes.list') return answer
      const doc = structuredClone(answer) as { volumes: { filesystem: string; capabilities: { snapshots: boolean } }[] }
      doc.volumes.forEach(volume => { volume.filesystem = 'ext4'; volume.capabilities.snapshots = false })
      return doc as never
    })
    const wrapper = mount(App, { attachTo: document.body })
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    ;[...document.body.querySelectorAll('button')].find(button => button.textContent?.includes('New scan'))?.click()
    await vi.waitFor(() => expect(document.body.textContent).toContain('A previous result is available'))
    const dialog = document.body.querySelector('[role="dialog"]')!
    expect(dialog.textContent).not.toMatch(/snapshots|reuses scan data/)
    expect(dialog.textContent).toContain('Ext4 always reads the selected folders again')
    wrapper.unmount()
  })
  beforeEach(() => {
    sessionStorage.clear()
    nav.page = 'overview'
    nav.followedJobId = null
    nav.newScanOpen = false
    jobs.list = []
    volumes.document = null
    volumes.selected = null
  })

  // The panel fits its content: the UI dialog keeps a growing panel inside
  // the window using its own measured height bounds.
  it('fits its content instead of taking the window body height', async () => {
    setBackend(fixtureBackend({ scenario: 'interaction' }))
    const wrapper = mount(App, { attachTo: document.body })
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    ;[...document.body.querySelectorAll('button')].find(button => button.textContent?.includes('New scan'))?.click()
    await vi.waitFor(() => expect(document.body.textContent).toContain('A previous result is available'))
    const panel = document.body.querySelector<HTMLElement>('[role="dialog"]')!
    expect(panel.style.height).toBe('')
    wrapper.unmount()
  })

  // The history estimate loads behind a mask over the setup, not a box under it.
  it('masks the setup while the scan history loads and keeps Start disabled', async () => {
    const recorded = fixtureBackend({ scenario: 'interaction' })
    let releaseHistory!: () => void
    const historyHeld = new Promise<void>(resolve => { releaseHistory = resolve })
    setBackend((async (method: Method, params: unknown) => {
      if (method === 'history.list') await historyHeld
      return recorded(method, params as never)
    }) as unknown as Backend)
    const wrapper = mount(App, { attachTo: document.body })
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    ;[...document.body.querySelectorAll('button')].find(button => button.textContent?.includes('New scan'))?.click()
    const dialog = await vi.waitFor(() => {
      const found = document.body.querySelector<HTMLElement>('[role="dialog"]')
      expect(found?.textContent).toContain('Choose a volume')
      return found!
    })
    const start = () => [...dialog.querySelectorAll('button')].find(button => button.textContent?.startsWith('Scan '))!

    const busy = dialog.querySelector('[aria-busy="true"]')
    expect(busy, dialog.innerHTML).not.toBeNull()
    expect(busy!.querySelector('#volume-heading')).not.toBeNull()
    // The mask waits out its delay, so a quick load never flashes it.
    const overlay = await vi.waitFor(() => {
      const found = busy!.querySelector('[data-loading-overlay]')
      expect(found).not.toBeNull()
      return found!
    })
    // The UI transition may wrap the overlay; it must still mask this setup
    expect(overlay.closest('[aria-busy="true"]')).toBe(busy)
    expect(start().disabled).toBe(true)

    releaseHistory()
    await vi.waitFor(() => expect(dialog.querySelector('[aria-busy="true"]')).toBeNull())
    expect(dialog.querySelector('[data-loading-overlay]')).toBeNull()
    expect(dialog.textContent).not.toContain('Loading')
    expect(start().disabled).toBe(false)
    wrapper.unmount()
  })

  it('links to the backend scan when another admin wins admission after this window loaded', async () => {
    const recorded = fixtureBackend({ scenario: 'no_report' })
    let refused = false
    setBackend((async (method: Method, params: unknown) => {
      if (method === 'scan.start') {
        refused = true
        throw new ApiError('scan_in_progress', 4, { running_job_id: 'other-admin-scan' })
      }
      if (method === 'jobs.list' && refused) {
        const response = await fixtureBackend({ scenario: 'scanning' })('jobs.list', {})
        response.jobs[0]!.job_id = 'other-admin-scan'
        return response as never
      }
      return recorded(method, params as never)
    }) as Backend)
    const wrapper = mount(App, { attachTo: document.body })
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    const buttons = () => [...document.body.querySelectorAll('button')]
    buttons().find(button => button.textContent?.includes('Scan volume1'))?.click()
    await flushPromises()
    expect(document.body.textContent).toContain('A scan is already running')
    buttons().find(button => button.textContent?.trim() === 'View progress')?.click()
    await flushPromises()
    expect(nav.followedJobId).toBe('other-admin-scan')
    expect(nav.page).toBe('scanning')
    wrapper.unmount()
  })

  it('starts a selected-share scan with the chosen profile through api()', async () => {
    const calls: { method: string; params: unknown }[] = []
    const recorded = fixtureBackend({ scenario: 'no_report' })
    setBackend((async (method: Method, params: unknown) => {
      calls.push({ method, params })
      return recorded(method, params as never)
    }) as unknown as Backend)

    const wrapper = mount(App, { attachTo: document.body })
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()

    const buttons = () => [...document.body.querySelectorAll('button')]
    const selectedShares = radio('Selected shared folders')
    expect(selectedShares).toBeDefined()
    selectedShares?.click()
    await flushPromises()
    const share = document.body.querySelector<HTMLButtonElement>('[role="checkbox"]')!
    expect(share).toBeTruthy()
    share.click()
    await flushPromises()
    radio('Fast')?.click()
    await flushPromises()
    const start = buttons().find(button => button.textContent?.includes('Scan 1 shared folder'))
    expect(start, document.body.textContent).toBeDefined()
    expect(start?.disabled).toBe(false)
    start?.click()
    await flushPromises()

    expect(calls.some(call => call.method === 'history.list')).toBe(true)
    expect(calls.find(call => call.method === 'scan.start')?.params).toEqual({
      targets: ['/volume1/share08'], mode: 'fast', snapshots: false,
    })
    await vi.waitFor(() => expect(nav.page).toBe('scanning'))
    wrapper.unmount()
  })

  it('resumes a recent paused scan with its original profile by default', async () => {
    vi.useFakeTimers({ toFake: ['Date'] })
    vi.setSystemTime(new Date('2026-09-30T01:04:20Z'))
    const calls: { method: string; params: unknown }[] = []
    const recorded = fixtureBackend({ scenario: 'paused' })
    setBackend((async (method: Method, params: unknown) => {
      calls.push({ method, params })
      return recorded(method, params as never)
    }) as unknown as Backend)

    const wrapper = mount(App, { attachTo: document.body })
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    const buttons = () => [...document.body.querySelectorAll('button')]
    buttons().find(button => button.textContent?.includes('New scan'))?.click()
    await flushPromises()

    expect(document.body.textContent).toContain('A paused scan is available on volume1')
    expect(buttons().find(button => button.textContent?.includes('Scan volume1'))?.disabled).toBe(true)
    buttons().find(button => button.textContent?.trim() === 'Resume')?.click()
    await flushPromises()
    const resumeButtons = buttons().filter(button => button.textContent?.trim() === 'Resume')
    resumeButtons.at(-1)?.click()
    await flushPromises()

    expect(calls.find(call => call.method === 'job.resume')?.params).toEqual({
      job_id: '20260930T010000Z-a1b2c', mode: 'gentle',
    })
    await vi.waitFor(() => expect(nav.page).toBe('scanning'))
    wrapper.unmount()
  })

  it('recovers a known incompatible paused job with all saved settings from New scan', async () => {
    vi.useFakeTimers({ toFake: ['Date'] })
    vi.setSystemTime(new Date('2026-09-30T01:05:00Z'))
    const recorded = fixtureBackend({ scenario: 'paused' })
    const sourceId = (await recorded('jobs.list', {})).jobs.find(job => job.state === 'paused')!.job_id
    const requests: unknown[] = []
    setBackend(async (method, params) => {
      if (method === 'job.resume') {
        requests.push(params)
        return { command: 'detach', job_id: 'fresh-incompatible-setup', exit: 0 } as never
      }
      const answer = await recorded(method, params)
      if (method !== 'jobs.list') return answer
      const doc = structuredClone(answer) as import('@/types').JobsDocument
      doc.jobs.find(job => job.job_id === sourceId)!.compatible = false
      return doc as never
    })
    const wrapper = mount(App, { attachTo: document.body })
    await flushPromises(); await vi.dynamicImportSettled(); await flushPromises()
    nav.newScanOpen = true
    await flushPromises(); await vi.dynamicImportSettled(); await flushPromises()
    expect(wrapper.find('[role="dialog"]').text()).toContain('The package was updated; scan again to see results')
    const button = wrapper.find('[role="dialog"]').findAll('button').find(button => button.text() === 'Start a scan')
    expect(button).toBeDefined()
    await button!.trigger('click')
    await vi.waitFor(() => expect(requests).toEqual([{ job_id: sourceId, fresh: true }]))
    expect(nav.followedJobId).toBe('fresh-incompatible-setup')
    expect(nav.newScanOpen).toBe(false)
    wrapper.unmount()
  })

  it('keeps a refused resume visible above New scan and returns to its recovery controls', async () => {
    vi.useFakeTimers({ toFake: ['Date'] })
    vi.setSystemTime(new Date('2026-09-30T01:04:20Z'))
    const recorded = fixtureBackend({ scenario: 'paused' })
    const reason = 'Saved subvolume identity changed; start a fresh scan'
    setBackend(async (method, params) => {
      if (method === 'job.resume') {
        if ((params as { fresh?: boolean }).fresh) return { command: 'detach', job_id: 'fresh-same-settings', exit: 0 } as never
        throw apiErrorFromCgi('checkpoint_unusable', reason)
      }
      return recorded(method, params)
    })
    const wrapper = mount(App, { attachTo: document.body })
    await flushPromises(); await vi.dynamicImportSettled(); await flushPromises()
    nav.newScanOpen = true
    await flushPromises()
    const setup = wrapper.find('[role="dialog"]')
    await setup.findAll('button').find(button => button.text() === 'Resume')!.trigger('click')
    await flushPromises()
    const dialog = () => wrapper.findAll('[role="dialog"]').find(dialog => dialog.text().startsWith('Resume scan'))!
    await dialog().findAll('button').find(button => button.text() === 'Resume')!.trigger('click')
    await flushPromises()
    expect(dialog().text()).toContain(reason)
    expect(nav.page).toBe('overview')
    await dialog().findAll('button').find(button => button.text() === 'Start a scan')!.trigger('click')
    await flushPromises()
    expect(wrapper.findAll('[role="dialog"]').some(dialog => dialog.text().startsWith('Resume scan'))).toBe(false)
    expect(nav.newScanOpen).toBe(false)
    expect(nav.followedJobId).toBe('fresh-same-settings')
    expect(nav.page).toBe('scanning')
    wrapper.unmount()
  })

  it('cancels a same-volume paused scan before starting over', async () => {
    vi.useFakeTimers({ toFake: ['Date'] })
    vi.setSystemTime(new Date('2026-09-30T01:04:20Z'))
    const calls: { method: string; params: unknown }[] = []
    const recorded = fixtureBackend({ scenario: 'paused' })
    setBackend((async (method: Method, params: unknown) => {
      calls.push({ method, params })
      return recorded(method, params as never)
    }) as unknown as Backend)

    const wrapper = mount(App, { attachTo: document.body })
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    const buttons = () => [...document.body.querySelectorAll('button')]
    buttons().find(button => button.textContent?.includes('New scan'))?.click()
    await flushPromises()
    buttons().find(button => button.textContent?.trim() === 'Start over')?.click()
    await flushPromises()

    await vi.waitFor(() => expect(calls.filter(call => ['job.cancel', 'scan.start'].includes(call.method)).map(call => call.method))
      .toEqual(['job.cancel', 'scan.start']))
    expect(calls.find(call => call.method === 'job.cancel')?.params).toEqual({ job_id: '20260930T010000Z-a1b2c' })
    expect(calls.find(call => call.method === 'scan.start')?.params).toEqual({
      targets: ['/volume1'], mode: 'gentle', snapshots: true,
    })
    await vi.waitFor(() => expect(nav.page).toBe('scanning'))
    wrapper.unmount()
  })

  // The system partition is not a scan target: New scan checks it again with the
  // same read as the System partition view's Check again, in the background.
  it('checks the system partition again from New scan while it has less than 900 MB free', async () => {
    const calls: { method: string; params: unknown }[] = []
    const recorded = fixtureBackend({ scenario: 'sysvol_getting_full' })
    setBackend((async (method: Method, params: unknown) => {
      calls.push({ method, params })
      return recorded(method, params as never)
    }) as unknown as Backend)
    const wrapper = mount(App, { attachTo: document.body })
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    nav.page = 'history'
    await flushPromises()
    const buttons = () => [...document.body.querySelectorAll('button')]
    buttons().find(button => button.textContent?.includes('New scan'))?.click()
    await setupLoaded()
    const system = radio('System partition')
    expect(system, document.body.textContent ?? '').toBeDefined()
    expect(system!.closest('label')!.textContent).toContain('886.9 MB free')
    system!.click()
    await flushPromises()
    const dialog = () => document.body.querySelector('[role="dialog"]')!
    expect(dialog().textContent).not.toContain('Scan scope')
    expect(dialog().textContent).not.toContain('Scan mode')
    expect(dialog().textContent).toContain('The check keeps running if you close this window')
    const before = calls.length
    buttons().find(button => button.textContent?.trim() === 'Check the system partition')?.click()
    await flushPromises()
    expect(nav.newScanOpen).toBe(false)
    expect(nav.page).toBe('overview')
    await vi.waitFor(() => expect(calls.slice(before).find(call => call.method === 'rootfs.report')?.params).toEqual({ max_age_s: 0 }))
    expect(calls.map(call => call.method)).not.toContain('scan.start')
    await vi.waitFor(() => expect(document.body.querySelector('.rootfs-view')?.textContent).toContain('What uses the space'))
    // The card reads the partition again once the check is done.
    await vi.waitFor(() => expect(calls.slice(before).map(call => call.method)).toContain('volumes.list'))
    await vi.waitFor(() => expect(volumes.loading).toBe(false))
    wrapper.unmount()
  })

  it('offers no system partition check while it has 900 MB free or more', async () => {
    setBackend(fixtureBackend({ scenario: 'interaction' }))
    const wrapper = mount(App, { attachTo: document.body })
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    ;[...document.body.querySelectorAll('button')].find(button => button.textContent?.includes('New scan'))?.click()
    await vi.waitFor(() => expect(document.body.textContent).toContain('A previous result is available'))
    expect(radio('System partition')).toBeUndefined()
    wrapper.unmount()
  })

  it('starts no scan when the authorization dialog is dismissed', async () => {
    const calls: string[] = []
    const recorded = fixtureBackend({ scenario: 'no_report' })
    setBackend((async (method: Method, params: unknown) => {
      calls.push(method)
      return recorded(method, params as never)
    }) as unknown as Backend)
    const wrapper = mount(App, { attachTo: document.body })
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    const undo = declineRootWork()
    ;[...document.body.querySelectorAll('button')].find(button => button.textContent?.includes('Scan volume1'))?.click()
    await flushPromises()
    expect(calls).not.toContain('scan.start')
    expect(nav.newScanOpen).toBe(true)
    expect(document.body.textContent).not.toContain('could not')
    undo()
    wrapper.unmount()
  })

  it('neither resumes, starts over nor cancels a paused scan when the authorization dialog is dismissed', async () => {
    vi.useFakeTimers({ toFake: ['Date'] })
    vi.setSystemTime(new Date('2026-09-30T01:04:20Z'))
    const calls: string[] = []
    const recorded = fixtureBackend({ scenario: 'paused' })
    setBackend((async (method: Method, params: unknown) => {
      calls.push(method)
      return recorded(method, params as never)
    }) as unknown as Backend)
    const wrapper = mount(App, { attachTo: document.body })
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    const undo = declineRootWork()
    const buttons = () => [...document.body.querySelectorAll('button')]
    buttons().find(button => button.textContent?.includes('New scan'))?.click()
    await flushPromises()
    buttons().find(button => button.textContent?.trim() === 'Resume')?.click()
    await flushPromises()
    buttons().filter(button => button.textContent?.trim() === 'Resume').at(-1)?.click()
    await flushPromises()
    buttons().find(button => button.textContent?.trim() === 'Start over')?.click()
    await flushPromises()
    buttons().find(button => button.textContent?.trim() === 'Cancel scan')?.click()
    await flushPromises()
    buttons().find(button => button.textContent?.trim() === 'Cancel paused scan')?.click()
    await flushPromises()
    expect(calls.filter(method => ['job.resume', 'job.cancel', 'scan.start'].includes(method))).toEqual([])
    undo()
    wrapper.unmount()
  })

  it('names a failed start with the API reason', async () => {
    const recorded = fixtureBackend({ scenario: 'no_report' })
    setBackend((async (method: Method, params: unknown) => {
      if (method === 'scan.start') throw new ApiError('timeout')
      return recorded(method, params as never)
    }) as unknown as Backend)
    const wrapper = mount(App, { attachTo: document.body })
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    const buttons = () => [...document.body.querySelectorAll('button')]
    buttons().find(button => button.textContent?.includes('Scan volume1'))?.click()
    await flushPromises()
    expect(document.body.textContent).toContain('The request took too long')
    wrapper.unmount()
  })

  it('gives the volume card the used space of its report, as the Overview card does', async () => {
    setBackend(fixtureBackend({ scenario: 'interaction' }))
    const wrapper = mount(App, { attachTo: document.body })
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    ;[...document.body.querySelectorAll('button')].find(button => button.textContent?.includes('New scan'))?.click()
    await vi.waitFor(() => expect(document.body.querySelector('[role="radiogroup"]')?.textContent).toContain('9.6 MB / 512 MB'))
    wrapper.unmount()
  })

  it('shows the captured volume size in the new-scan picker', async () => {
    const recorded = fixtureBackend()
    let releaseOverview!: () => void
    const overviewReady = new Promise<void>(resolve => { releaseOverview = resolve })
    setBackend(async (method, params) => {
      const answer = await recorded(method, params)
      if (method === 'overview.get') await overviewReady
      return answer
    })
    const wrapper = mount(App, { attachTo: document.body })
    try {
      await flushPromises()
      await vi.dynamicImportSettled()
      await flushPromises()
      ;[...document.body.querySelectorAll('button')].find(button => button.textContent?.includes('New scan'))?.click()
      await setupLoaded()
      const usage = ({ used_bytes, total_bytes }: { used_bytes: number; total_bytes: number }) =>
        `${formatFileSize(used_bytes)} / ${formatFileSize(total_bytes)}`
      const liveUsage = usage(capturedVolumes.volumes[0]!)
      const reportUsage = usage(capturedOverview.results[0]!.space.df)
      expect(liveUsage).not.toBe(reportUsage)
      const picker = () => document.body.querySelector('[role="radiogroup"]')?.textContent
      // The optional report arrives independently: the live capacity is valid
      // until then. Both readings use DSM's 1024-based public formatter.
      expect(picker()).toContain(liveUsage)
      releaseOverview()
      await flushPromises()
      await vi.dynamicImportSettled()
      await flushPromises()
      expect(picker()).toContain(reportUsage)
      expect(picker()).not.toContain(liveUsage)
    } finally {
      releaseOverview()
      wrapper.unmount()
      await flushPromises()
    }
  })

  it('says a rescan reuses valid data, and that gentle can still affect performance', async () => {
    setBackend(fixtureBackend({ scenario: 'interaction' }))
    const wrapper = mount(App, { attachTo: document.body })
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    ;[...document.body.querySelectorAll('button')].find(button => button.textContent?.includes('New scan'))?.click()
    await vi.waitFor(() => expect(document.body.textContent).toContain('A previous result is available'))

    const text = document.body.textContent ?? ''
    expect(text).toContain('A new scan reuses scan data that is still valid')
    expect(text).toContain('Lower priority; it may still affect NAS performance')
    wrapper.unmount()
  })

  it('caps the shared-folder selection at the 256-target limit', async () => {
    const base = fixtureBackend({ scenario: 'normal' })
    setBackend((async (method: Method, params: unknown) => {
      const result = await base(method, params as never)
      if (method !== 'volumes.list') return result
      const doc = structuredClone(result) as { volumes: { shares: unknown[] }[] }
      doc.volumes[0].shares = Array.from({ length: 300 }, (_, index) => ({
        name: { path: `share${String(index).padStart(3, '0')}` },
        path: { path: `/volume1/share${String(index).padStart(3, '0')}` },
        subvolume_id: 500 + index,
      }))
      return doc as never
    }) as unknown as Backend)

    const wrapper = mount(App, { attachTo: document.body })
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    const buttons = () => [...document.body.querySelectorAll('button')]
    buttons().find(button => button.textContent?.includes('New scan'))?.click()
    await setupLoaded()

    // Once loaded, the dialog opens on the whole volume with nothing selected, never blocked.
    expect(buttons().find(button => button.textContent?.trim() === 'Scan volume1')?.disabled).toBe(false)

    radio('Selected shared folders')?.click()
    await flushPromises()
    expect(document.body.textContent).toContain('Too many shared folders')

    // Select all cannot go past the WebAPI limit, so a 300-share volume cannot
    // land the user on a blocked Start button.
    buttons().find(button => button.textContent?.includes('Select all shown'))?.click()
    await flushPromises()
    expect(document.body.textContent).toContain('256 of 256 selected')
    expect(buttons().find(button => button.textContent?.includes('Scan 256 shared folders'))?.disabled).toBe(false)
    wrapper.unmount()
  })

  it('does not offer a paused scan older than seven days (D-4)', async () => {
    vi.useFakeTimers({ toFake: ['Date'] })
    vi.setSystemTime(Date.parse('2026-10-08T02:00:00Z'))
    setBackend(fixtureBackend({ scenario: 'paused' }))
    const wrapper = mount(App, { attachTo: document.body })
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    ;[...document.body.querySelectorAll('button')].find(button => button.textContent?.includes('New scan'))?.click()
    await flushPromises()
    expect(document.body.textContent).toContain('Scan volume1')
    expect(radio('volume1')).toBeTruthy()
    expect(document.body.textContent).not.toContain('A paused scan is available')
    wrapper.unmount()
    vi.useRealTimers()
  })

  it('closes from a plain Cancel button without starting a scan', async () => {
    const calls: string[] = []
    const recorded = fixtureBackend({ scenario: 'no_report' })
    setBackend((async (method: Method, params: unknown) => {
      calls.push(method)
      return recorded(method, params as never)
    }) as unknown as Backend)
    const wrapper = mount(App, { attachTo: document.body })
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    if (!document.body.querySelector('[role="dialog"]')) {
      ;[...document.body.querySelectorAll('button')].find(button => button.textContent?.includes('New scan'))?.click()
      await flushPromises()
    }
    const cancel = [...document.body.querySelectorAll<HTMLButtonElement>('[role="dialog"] button')]
      .find(button => button.textContent?.trim() === 'Cancel')
    expect(cancel, document.body.textContent ?? undefined).toBeDefined()
    cancel!.click()
    await flushPromises()
    expect(calls).not.toContain('scan.start')
    expect(nav.newScanOpen).toBe(false)
    wrapper.unmount()
  })

  it('names the volumes it cannot scan because they are not Btrfs', async () => {
    setBackend(fixtureBackend({ scenario: 'unsupported_skipped' }))
    nav.newScanOpen = true
    const wrapper = mount(App, { attachTo: document.body })
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    const note = document.body.querySelector('.scan-setup [data-testid="unsupported-volumes"]')
    expect(note?.textContent).toContain('/volume2')
    expect(note?.textContent).toContain('These file systems cannot be scanned')
    wrapper.unmount()
  })
})
