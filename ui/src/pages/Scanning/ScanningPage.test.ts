import { flushPromises, mount } from '@vue/test-utils'
import { ApiError, setBackend } from '@/api'
import { apiErrorFromCgi } from '@/api/errors'
import { fixtureBackend } from '@/api/fixtures'
import App from '@/App.vue'
import { allowAuthorizationForTests, declineRootWork } from '@/authorizationFixture'
import { jobs, refreshJobs } from '@/stores/jobs'
import { nav } from '@/stores/nav'
import { volumes } from '@/stores/volumes'

// Mounting App.vue needs the startup gate ready; the gate itself is tested separately.
allowAuthorizationForTests()

// Date-only fixture clocks must not leak into later tests.
afterEach(() => vi.useRealTimers())

describe('ScanningPage', () => {
  it('hides unsupported metadata and Btrfs phases for an ext4 scan', async () => {
    const recorded = fixtureBackend({ scenario: 'scan_test' })
    setBackend(async (method, params) => {
      const answer = await recorded(method, params)
      if (method !== 'volumes.list') return answer
      const doc = structuredClone(answer) as { volumes: { filesystem: string }[] }
      doc.volumes.forEach(volume => { volume.filesystem = 'ext4' })
      return doc as never
    })
    const wrapper = mount(App, { attachTo: document.body })
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    expect(wrapper.text()).not.toContain('Metadata read')
    expect(wrapper.find('.phase-list').text()).not.toMatch(/subvolumes|snapshots|shared blocks/)
    const live = jobs.list.find(job => job.kind === 'scan' && job.state === 'running')!
    live.phase = 'aggregate'
    live.progress = { ...live.progress!, phase: 'aggregate', entries: 300, items: 12_345 }
    await flushPromises()
    const processed = () => wrapper.findAll('.stat-card').find(card => card.text().includes('Records processed'))
    expect(processed()?.text()).toContain('12,345')
    live.progress.items = 23_456
    await flushPromises()
    expect(processed()?.text()).toContain('23,456')
    expect(wrapper.findAll('.stat-card').find(card => card.text().includes('Entries read'))?.text()).toContain('300')
    wrapper.unmount()
  })
  beforeEach(() => {
    document.body.innerHTML = ''
    sessionStorage.clear()
    jobs.cancelRequested = []
    nav.page = 'scanning'
    nav.newScanOpen = false
    jobs.list = []
    volumes.document = null
    volumes.selected = null
  })

  it('explains an unusable checkpoint and offers an explicit fresh scan', async () => {
    const recorded = fixtureBackend({ scenario: 'scan_test' })
    nav.followedJobId = (await recorded('jobs.list', {})).jobs.find(job => job.kind === 'scan')!.job_id
    const calls: string[] = []
    setBackend(async (method, params) => {
      calls.push(method)
      if (method === 'job.resume') return { command: 'detach', job_id: 'fresh-failed-scan', exit: 0 } as never
      const answer = await recorded(method, params)
      if (method !== 'jobs.list') return answer
      const doc = structuredClone(answer) as typeof answer & { jobs: import('@/types').Job[] }
      const job = doc.jobs.find(job => job.kind === 'scan')!
      job.state = 'failed'
      job.result = { completion: 'failed', exit_code: 1, reports: [], error_codes: ['checkpoint_unusable'], document: { path: '/private/result.json' } }
      return doc as never
    })
    const wrapper = mount(App, { attachTo: document.body })
    await flushPromises(); await vi.dynamicImportSettled(); await flushPromises()
    expect(wrapper.text()).toContain('This scan cannot resume')
    expect(wrapper.text()).toContain('Start a fresh scan')
    expect(calls).not.toContain('job.resume')
    expect(calls).not.toContain('scan.start')
    const button = wrapper.findAll('button').find(button => button.text() === 'Start a scan')!
    await button.trigger('click')
    await flushPromises()
    await vi.waitFor(() => expect(nav.followedJobId).toBe('fresh-failed-scan'))
    expect(nav.newScanOpen).toBe(false)
    wrapper.unmount()
  }, 30000)

  it.each(['paused', 'interrupted'] as const)('recovers a known incompatible %s job in one click with its saved settings', async state => {
    vi.useFakeTimers({ toFake: ['Date'] })
    vi.setSystemTime(new Date('2026-09-30T01:05:00Z'))
    const recorded = fixtureBackend({ scenario: 'paused' })
    const sourceId = (await recorded('jobs.list', {})).jobs.find(job => job.state === 'paused')!.job_id
    nav.followedJobId = sourceId
    const requests: unknown[] = []
    setBackend(async (method, params) => {
      if (method === 'job.resume') {
        requests.push(params)
        return { command: 'detach', job_id: 'fresh-incompatible', exit: 0 } as never
      }
      const answer = await recorded(method, params)
      if (method !== 'jobs.list') return answer
      const doc = structuredClone(answer) as import('@/types').JobsDocument
      Object.assign(doc.jobs.find(job => job.job_id === sourceId)!, { state, compatible: false })
      return doc as never
    })
    const wrapper = mount(App, { attachTo: document.body })
    await flushPromises(); await vi.dynamicImportSettled(); await flushPromises()
    expect(wrapper.text()).toContain('The package was updated; scan again to see results')
    const button = wrapper.findAll('button').find(button => button.text() === 'Start a scan')
    expect(button).toBeDefined()
    await button!.trigger('click')
    await vi.waitFor(() => expect(requests).toEqual([{ job_id: sourceId, fresh: true }]))
    expect(nav.followedJobId).toBe('fresh-incompatible')
    expect(nav.newScanOpen).toBe(false)
    wrapper.unmount()
  })

  it('resumes the paused job ID without starting a fresh scan', async () => {
    vi.useFakeTimers({ toFake: ['Date'] })
    vi.setSystemTime(new Date('2026-09-30T01:05:00Z'))
    const recorded = fixtureBackend({ scenario: 'paused' })
    const paused = await recorded('jobs.list', {})
    nav.followedJobId = paused.jobs.find(job => job.state === 'paused')!.job_id
    const calls: { method: string; params: unknown }[] = []
    setBackend(async (method, params) => {
      calls.push({ method, params })
      if (method === 'job.resume') return { schema_version: 1, command: 'detach', completion: 'complete', job_id: (params as { job_id: string }).job_id, run: [], errors: [], warnings: [] } as never
      return recorded(method, params)
    })
    const wrapper = mount(App, { attachTo: document.body })
    await flushPromises(); await vi.dynamicImportSettled(); await flushPromises()
    const job = jobs.list.find(job => job.state === 'paused')!
    expect(job).toBeTruthy()
    nav.page = 'scanning'
    nav.followedJobId = job.job_id
    await flushPromises(); await vi.dynamicImportSettled(); await flushPromises()
    expect(wrapper.text()).toContain('Resume')
    await wrapper.findAll('button').find(button => button.text() === 'Resume')!.trigger('click')
    await flushPromises()
    const buttons = wrapper.findAll('button').filter(button => button.text() === 'Resume')
    await buttons[buttons.length - 1]!.trigger('click')
    await flushPromises()
    expect(calls.filter(call => call.method === 'job.resume').map(call => call.params)).toEqual([{ job_id: job.job_id, mode: 'gentle' }])
    expect(calls.some(call => ['scan.start', 'scans.start'].includes(call.method))).toBe(false)
    wrapper.unmount()
  }, 30000)

  it.each(['checkpoint_unusable', 'ROOT_BUSY'])('shows a refused resume inside its dialog with recovery (%s)', async cgiCode => {
    vi.useFakeTimers({ toFake: ['Date'] })
    vi.setSystemTime(new Date('2026-09-30T01:05:00Z'))
    const recorded = fixtureBackend({ scenario: 'paused' })
    const sourceId = (await recorded('jobs.list', {})).jobs.find(job => job.state === 'paused')!.job_id
    nav.followedJobId = sourceId
    const reason = 'Saved subvolume identity changed; start a fresh scan'
    let attempts = 0
    let freshRequest: unknown
    setBackend(async (method, params) => {
      if (method === 'job.resume') {
        if ((params as { fresh?: boolean }).fresh) {
          freshRequest = params
          return { command: 'detach', job_id: 'fresh-scan', exit: 0 } as never
        }
        attempts++
        throw apiErrorFromCgi(cgiCode, reason)
      }
      return recorded(method, params)
    })
    const wrapper = mount(App, { attachTo: document.body })
    await flushPromises(); await vi.dynamicImportSettled(); await flushPromises()
    await wrapper.findAll('button').find(button => button.text() === 'Resume')!.trigger('click')
    await flushPromises()
    const dialog = () => wrapper.find('[role="dialog"]')
    await dialog().findAll('button').find(button => button.text() === 'Resume')!.trigger('click')
    await flushPromises()
    expect(dialog().text()).toContain('Scan action failed')
    expect(dialog().text()).toContain(cgiCode === 'checkpoint_unusable' ? reason : 'Syno Reclaim is handling other requests')
    expect(jobs.list.find(job => job.job_id === nav.followedJobId)?.state).toBe('paused')
    // A transient failure can be retried with the same job, without closing the dialog.
    await dialog().findAll('button').find(button => button.text() === 'Resume')!.trigger('click')
    await flushPromises()
    expect(attempts).toBe(2)
    await dialog().findAll('button').find(button => button.text() === 'Start a scan')!.trigger('click')
    await flushPromises()
    expect(freshRequest).toEqual({ job_id: sourceId, fresh: true })
    expect(nav.followedJobId).toBe('fresh-scan')
    expect(nav.newScanOpen).toBe(false)
    expect(wrapper.findAll('[role="dialog"]').some(dialog => dialog.text().includes('Resume scan'))).toBe(false)
    wrapper.unmount()
  })

  it('continues the same scan after business writes and shows changed-unit work separately', async () => {
    vi.useFakeTimers({ toFake: ['Date'] })
    vi.setSystemTime(new Date('2026-09-30T01:05:00Z'))
    const recorded = fixtureBackend({ scenario: 'paused' })
    const original = (await recorded('jobs.list', {})).jobs.find(job => job.state === 'paused')!
    nav.followedJobId = original.job_id
    let resumed = false
    let reread = 0
    const calls: { method: string; params: unknown }[] = []
    setBackend(async (method, params) => {
      calls.push({ method, params })
      if (method === 'job.resume') {
        resumed = true
        return { command: 'detach', job_id: original.job_id, exit: 0 } as never
      }
      const answer = await recorded(method, params)
      if (method !== 'jobs.list' || !resumed) return answer
      const doc = structuredClone(answer) as import('@/types').JobsDocument
      const job = doc.jobs.find(job => job.job_id === original.job_id)!
      reread += 80
      job.state = 'running'
      job.resumes++
      job.progress = { ...original.progress!,
        items: original.progress!.items + reread, bytes_read: original.progress!.bytes_read + reread * 32,
        restart: { subvolume_id: 256, entries: 40, items: reread, bytes_read: reread * 32 } }
      return doc as never
    })
    const wrapper = mount(App, { attachTo: document.body })
    await flushPromises(); await vi.dynamicImportSettled(); await flushPromises()
    await wrapper.findAll('button').find(button => button.text() === 'Resume')!.trigger('click')
    await flushPromises()
    await wrapper.find('[role="dialog"]').findAll('button').find(button => button.text() === 'Resume')!.trigger('click')
    await flushPromises()
    await refreshJobs(); await flushPromises()
    expect(wrapper.find('[role="dialog"]').exists()).toBe(false)
    expect(wrapper.text()).toContain('This folder changed while paused')
    expect(wrapper.text()).toContain('Other folders keep their progress')
    let current = jobs.list.find(job => job.job_id === original.job_id)!
    expect(current.state).toBe('running')
    expect(current.progress!.entries).toBe(original.progress!.entries)
    const first = current.progress!.items
    await refreshJobs(); await flushPromises()
    current = jobs.list.find(job => job.job_id === original.job_id)!
    expect(current.progress!.items).toBeGreaterThan(first)
    expect(current.progress!.restart!.items).toBeLessThan(current.progress!.items)
    expect(calls.filter(call => call.method === 'job.resume').map(call => call.params)).toEqual([{ job_id: original.job_id, mode: 'gentle' }])
    expect(calls.some(call => call.method === 'scan.start')).toBe(false)
    wrapper.unmount()
  })

  it('shows the share of the last scan\'s metadata items read', async () => {
    setBackend(fixtureBackend({ scenario: 'scan_test' }))
    const wrapper = mount(App, { attachTo: document.body })
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()

    expect(wrapper.find('.ring-percent').text()).toBe('35%')
    expect(wrapper.find('.progress-ring [role="progressbar"]').attributes('aria-valuenow')).toBe('35')
    expect(wrapper.text()).toContain('Entries read')
    expect(wrapper.text()).toContain('Current item: share01')
    expect(wrapper.text()).toContain('Pause')
    expect(wrapper.text()).toContain('Cancel scan')
    expect(wrapper.find('.job-strip').exists()).toBe(false)
    wrapper.unmount()
  })

  // scan.start answers before jobs.list lists the new job; for about 5 s the
  // page said no scan was running under a Scanning title.
  it('says the scan is starting until jobs.list lists the job it follows', async () => {
    vi.useFakeTimers({ toFake: ['Date'] })
    vi.setSystemTime(new Date('2026-10-14T10:00:00Z'))
    setBackend(fixtureBackend({ scenario: 'normal' }))
    nav.page = 'scanning'
    nav.followedJobId = 'scan-just-started'
    const wrapper = mount(App, { attachTo: document.body })
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    expect(wrapper.text()).not.toContain('No scan is running')
    expect(wrapper.text()).toContain('Starting the scan')

    // A job that never shows up is not waited for forever.
    vi.setSystemTime(new Date('2026-10-14T10:00:31Z'))
    await refreshJobs()
    await new Promise(resolve => setTimeout(resolve, 1100))
    await flushPromises()
    expect(wrapper.text()).toContain('No scan is running')
    wrapper.unmount()
    nav.followedJobId = null
  })

  it('shows the real scan progress and whole-volume name', async () => {
    setBackend(fixtureBackend({ scenario: 'scanning' }))
    const wrapper = mount(App, { attachTo: document.body })
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    expect(wrapper.find('.scan-title h2').text()).toBe('Scanning: Whole volume1')
    expect(wrapper.text()).toContain('5,371,562')
    wrapper.unmount()
  })

  it('shrinks a long ring count so it keeps clear padding inside the ring', async () => {
    setBackend(fixtureBackend({ scenario: 'scanning' }))
    const wrapper = mount(App, { attachTo: document.body })
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    expect(wrapper.find('.ring-count').text()).toBe('5,371,562')
    expect(wrapper.find('.ring-count').classes()).toContain('ring-long')
    wrapper.unmount()
  })

  it('names the share a share scan reads and counts its entries', async () => {
    setBackend(fixtureBackend({ scenario: 'share_scanning' }))
    const wrapper = mount(App, { attachTo: document.body })
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    expect(wrapper.find('.scan-title h2').text()).toBe('Scanning: share04')
    expect(wrapper.text()).toContain('Current item: share04')
    expect(wrapper.text()).toContain('184,000')
    expect(wrapper.find('.ring-percent').text()).toBe('40%')
    wrapper.unmount()
  })

  it('names the first share and the count of the others', async () => {
    const recorded = fixtureBackend({ scenario: 'share_scanning' })
    setBackend(async (method, params) => {
      const answer = await recorded(method, params)
      if (method !== 'jobs.list') return answer
      const list = answer as { jobs: object[] }
      const targets = [{ path: '/volume1/share04' }, { path: '/volume1/share06' }]
      return { ...list, jobs: list.jobs.map(job => ({ ...job, targets })) } as never
    })
    const wrapper = mount(App, { attachTo: document.body })
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    expect(wrapper.find('.scan-title h2').text()).toBe('Scanning: share04 and 1 more')
    wrapper.unmount()
  })

  it('does not treat a cleanup result as a scan result', async () => {
    const recorded = fixtureBackend({ scenario: 'scan_test' })
    let completed = false
    const calls: string[] = []
    setBackend(async (method, params) => {
      calls.push(method)
      if (method === 'jobs.list') {
        const document = await recorded(method, params) as { jobs: typeof jobs.list }
        if (!completed) return document as never
        return {
          ...document,
          jobs: document.jobs.map(job => job.job_id === '20260930T010000Z-a1b2c' ? { ...job, state: 'completed' as const } : job),
        } as never
      }
      if (method === 'job.result') {
        return {
          schema_version: 1,
          command: 'cleanup-apply',
          tool_version: 'test',
          generated_at: '2026-09-30T00:00:00Z',
          completion: 'complete',
          job_id: '20260930T010000Z-a1b2c',
          errors: [],
          warnings: [],
        } as never
      }
      return recorded(method, params)
    })
    const wrapper = mount(App, { attachTo: document.body })
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    await vi.waitFor(() => expect(jobs.list[0]?.state).toBe('running'))
    completed = true
    await refreshJobs()
    await vi.waitFor(() => expect(calls).toContain('job.result'))

    expect(nav.page).toBe('scanning')
    wrapper.unmount()
  })

  async function mountWith(failing: string) {
    const recorded = fixtureBackend({ scenario: 'scan_test' })
    setBackend(async (method, params) => {
      if (method === failing) throw new ApiError('timeout')
      return recorded(method, params)
    })
    const wrapper = mount(App, { attachTo: document.body })
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    return wrapper
  }

  it('names a failed pause with the API reason', async () => {
    const wrapper = await mountWith('job.pause')
    await wrapper.findAll('.scan-actions button').find(b => b.text() === 'Pause')!.trigger('click')
    await flushPromises()
    expect(wrapper.text()).toContain('The request took too long')
    wrapper.unmount()
  })

  it('sends no job request when the authorization dialog is dismissed', async () => {
    const calls: string[] = []
    const recorded = fixtureBackend({ scenario: 'scan_test' })
    setBackend(async (method, params) => {
      calls.push(method)
      return recorded(method, params)
    })
    const wrapper = mount(App, { attachTo: document.body })
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    const undo = declineRootWork()
    await wrapper.findAll('.scan-actions button').find(b => b.text() === 'Pause')!.trigger('click')
    await flushPromises()
    expect(calls).not.toContain('job.pause')
    expect(wrapper.text()).not.toContain('Scan action failed')
    undo()
    wrapper.unmount()
  })

  it('names a failed job list with the API reason', async () => {
    const wrapper = await mountWith('jobs.list')
    expect(wrapper.text()).toContain('The request took too long')
    wrapper.unmount()
  })

  it('says why a pause was refused', async () => {
    const recorded = fixtureBackend({ scenario: 'scan_test' })
    setBackend(async (method, params) => {
      if (method !== 'job.pause') return recorded(method, params)
      const answer = await recorded(method, params)
      return { ...answer, outcome: 'finished', state: 'completed' } as never
    })
    const wrapper = mount(App, { attachTo: document.body })
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    await wrapper.findAll('.scan-actions button').find(b => b.text() === 'Pause')!.trigger('click')
    await flushPromises()
    await vi.waitFor(() => expect(wrapper.text()).toContain('The scan has already ended'))
    expect(wrapper.text()).not.toContain('Scan action failed')
    wrapper.unmount()
  })

  it('leaves the job strip to the other pages', async () => {
    nav.page = 'overview'
    setBackend(fixtureBackend({ scenario: 'scan_test' }))
    const wrapper = mount(App, { attachTo: document.body })
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    expect(wrapper.find('.job-strip').text().match(/%/g)).toHaveLength(1)
    wrapper.unmount()
  })

  it('shows the entries read, not a percentage, in a strip without a basis', async () => {
    nav.page = 'overview'
    const recorded = fixtureBackend({ scenario: 'scan_test' })
    setBackend(async (method, params) => {
      const answer = await recorded(method, params)
      if (method !== 'jobs.list') return answer
      const list = answer as { jobs: { progress?: object }[] }
      return { ...list, jobs: list.jobs.map(job => (job.progress ? { ...job, progress: { ...job.progress, basis: null } } : job)) } as never
    })
    const wrapper = mount(App, { attachTo: document.body })
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    const strip = wrapper.find('.job-strip').text()
    expect(strip).toContain('1,250,000 entries read')
    expect(strip).not.toContain('%')
  })

  it('says the progress stalled once it is 30 s old, without a new poll', async () => {
    vi.useFakeTimers({ toFake: ['Date', 'setInterval', 'clearInterval'] })
    vi.setSystemTime(Date.parse('2026-09-30T01:04:20Z'))
    setBackend(fixtureBackend({ scenario: 'scan_test' }))
    const wrapper = mount(App, { attachTo: document.body })
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    expect(wrapper.text()).not.toContain("progress has not updated")
    vi.advanceTimersByTime(25_000)
    await flushPromises()
    expect(wrapper.text()).toContain("progress has not updated")
    wrapper.unmount()
    vi.useRealTimers()
  })

  it('keeps saying "Cancelling" while the CLI reports a pending cancel as pausing (F6)', async () => {
    const recorded = fixtureBackend({ scenario: 'scan_test' })
    let cancelled = false
    setBackend(async (method, params) => {
      const answer = await recorded(method, params)
      if (method === 'job.cancel') {
        cancelled = true
        return { ...answer, outcome: 'requested', state: 'pausing' } as never
      }
      if (method === 'jobs.list' && cancelled) {
        const list = answer as { jobs: { state: string }[] }
        return { ...list, jobs: list.jobs.map((job, i) => (i === 0 ? { ...job, state: 'pausing' } : job)) } as never
      }
      return answer
    })
    const wrapper = mount(App, { attachTo: document.body })
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    await wrapper.findAll('.scan-actions button').find(b => b.text() === 'Cancel scan')!.trigger('click')
    await flushPromises()
    ;[...document.body.querySelectorAll<HTMLButtonElement>('[role="dialog"] button')].find(b => b.textContent?.trim() === 'Cancel scan')!.click()
    await vi.waitFor(() => expect(wrapper.find('.scan-title h2').text()).toBe('Cancelling scan: Whole volume1'))
    wrapper.unmount()
  })

  it.each(['cancelled', 'interrupted', 'failed'] as const)('distinguishes a known %s outcome from a missing final result', async (state) => {
    vi.useFakeTimers({ toFake: ['Date'] })
    vi.setSystemTime(new Date('2026-09-30T01:04:20Z'))
    nav.followedJobId = '20260930T010000Z-a1b2c'
    const recorded = fixtureBackend({ scenario: 'scan_test' })
    setBackend(async (method, params) => {
      const answer = await recorded(method, params)
      if (method !== 'jobs.list') return answer
      const list = answer as { jobs: typeof jobs.list }
      return { ...list, jobs: list.jobs.map((job, i) => i === 0 ? { ...job, state, result: null } : job) } as never
    })
    const wrapper = mount(App, { attachTo: document.body })
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    if (state === 'cancelled') {
      expect(wrapper.text()).toContain('Scan cancelled')
      expect(wrapper.text()).not.toContain('final outcome is unknown')
      expect(wrapper.text()).not.toContain('Operation interrupted')
    } else {
      expect(wrapper.text()).toContain('Operation interrupted')
    }
    wrapper.unmount()
    nav.followedJobId = null
  })

  it('explains publishing without stale progress, an ETA or a mode action', async () => {
    vi.useFakeTimers({ toFake: ['Date'] })
    vi.setSystemTime(Date.parse('2026-09-30T01:10:00Z'))
    setBackend(fixtureBackend({ scenario: 'scan_test' }))
    const wrapper = mount(App, { attachTo: document.body })
    await flushPromises(); await vi.dynamicImportSettled(); await flushPromises()
    jobs.list = jobs.list.map(job => ({ ...job, phase: 'publish', progress: { ...job.progress!, phase: 'publish' } }))
    await flushPromises()
    expect(wrapper.find('.scan-eta').text()).toBe('Saving results')
    expect(wrapper.text()).not.toContain('progress has not updated')
    expect(wrapper.text()).toContain('Changing mode does not affect saving results')
    expect(wrapper.findAll('.scan-actions button').some(button => button.text() === 'Switch to fast')).toBe(false)
    expect(wrapper.find('.ring-percent').exists()).toBe(false)
    jobs.list = jobs.list.map(job => ({ ...job, state: 'failed', result: null }))
    await flushPromises()
    expect(wrapper.text()).toContain('Operation interrupted')
    wrapper.unmount()
  })

  /** The recorded scanning set, clicking Switch to fast with job.mode answering `outcome`. */
  async function mountChangingMode(outcome = 'requested', holdMode = false) {
    const recorded = fixtureBackend({ scenario: 'scan_test' })
    const calls: { method: string; params: unknown }[] = []
    let confirmedProfile = 'low'
    setBackend(async (method, params) => {
      calls.push({ method, params })
      const answer = await recorded(method, params)
      if (method === 'jobs.list' && holdMode) {
        const list = answer as { jobs: typeof jobs.list }
        return { ...list, jobs: list.jobs.map(job => ({ ...job, profile: confirmedProfile })) } as never
      }
      return method === 'job.mode' ? ({ ...answer, outcome } as never) : answer
    })
    const wrapper = mount(App, { attachTo: document.body })
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    await wrapper.findAll('.scan-actions button').find(b => b.text() === 'Switch to fast')!.trigger('click')
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    return { wrapper, calls, confirmProfile: async () => {
      confirmedProfile = 'fast'
      await refreshJobs()
      await flushPromises()
    } }
  }

  it('changes the mode of the running scan in place: no pause, no resume, no restart', async () => {
    const { wrapper, calls } = await mountChangingMode()
    const modes = calls.filter(call => call.method === 'job.mode')
    expect(modes.map(call => call.params)).toEqual([{ job_id: '20260930T010000Z-a1b2c', mode: 'fast' }])
    expect(calls.some(call => ['job.pause', 'job.resume', 'job.cancel'].includes(call.method))).toBe(false)
    expect(wrapper.text()).not.toContain('Pausing scan')
    wrapper.unmount()
  })

  it('keeps a mode change pending until the polled profile changes', async () => {
    const { wrapper, confirmProfile } = await mountChangingMode('requested', true)
    expect(wrapper.text()).toContain('Changing mode…')
    const button = wrapper.findAll('.scan-actions button').find(button => button.text() === 'Changing mode…')!
    expect(button.attributes('disabled')).toBeDefined()
    await confirmProfile()
    expect(wrapper.text()).not.toContain('Changing mode…')
    expect(wrapper.text()).toContain('Switch to gentle')
    wrapper.unmount()
  })

  it('keeps a slow mode change pending until its profile is confirmed', async () => {
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] })
    const { wrapper, confirmProfile } = await mountChangingMode('requested', true)
    await vi.advanceTimersByTimeAsync(60_000)
    expect(wrapper.text()).not.toContain('The mode change is not confirmed yet; the scan continues in the displayed mode')
    expect(wrapper.text()).toContain('Changing mode…')
    await confirmProfile()
    expect(wrapper.text()).not.toContain('Changing mode…')
    expect(wrapper.text()).toContain('Switch to gentle')
    wrapper.unmount()
  })

  it('explains a mode request that reaches the backend during publishing', async () => {
    const { wrapper } = await mountChangingMode('too_late')
    expect(wrapper.text()).toContain('Changing mode does not affect saving results')
    wrapper.unmount()
  })

  it('says when the scan ended before its mode could change', async () => {
    const { wrapper } = await mountChangingMode('finished')
    await vi.waitFor(() => expect(wrapper.text()).toContain('The scan has already ended'))
    wrapper.unmount()
  })

  it('says when the job is no scan whose mode can change', async () => {
    const { wrapper } = await mountChangingMode('not_adjustable')
    await vi.waitFor(() => expect(wrapper.text()).toContain('Only a scan can change its mode'))
    wrapper.unmount()
  })

  it('counts the elapsed time up every second between polls and holds it while paused', async () => {
    vi.useFakeTimers({ toFake: ['Date', 'setInterval', 'clearInterval'] })
    vi.setSystemTime(Date.parse('2026-09-30T01:04:20Z'))
    setBackend(fixtureBackend({ scenario: 'scan_test' }))
    const wrapper = mount(App, { attachTo: document.body })
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    const elapsed = () => wrapper.findAll('.stat-card').find(card => card.text().startsWith('Elapsed time'))!.find('[data-ui-statistic-value]').text()
    expect(elapsed()).toBe('4:10')
    vi.advanceTimersByTime(1_000)
    await flushPromises()
    expect(elapsed()).toBe('4:11')
    vi.advanceTimersByTime(2_000)
    await flushPromises()
    expect(elapsed()).toBe('4:13')
    jobs.list = jobs.list.map(job => ({ ...job, state: 'paused' }))
    await flushPromises()
    expect(elapsed()).toBe('4:10')
    vi.advanceTimersByTime(5_000)
    await flushPromises()
    expect(elapsed()).toBe('4:10')
    wrapper.unmount()
  })

  it('says why and when the scan paused, in shared folders', async () => {
    vi.useFakeTimers({ toFake: ['Date'] })
    vi.setSystemTime(new Date('2026-09-30T01:04:20Z'))
    setBackend(fixtureBackend({ scenario: 'paused' }))
    const wrapper = mount(App, { attachTo: document.body })
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    expect(wrapper.text()).toContain('Paused by an administrator')
    expect(wrapper.text()).toContain('3 shared folders done')
    wrapper.unmount()
  })

  it('keeps the ring to the percentage and moves the ETA under the title', async () => {
    setBackend(fixtureBackend({ scenario: 'scan_test' }))
    const wrapper = mount(App, { attachTo: document.body })
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    expect(wrapper.find('.ring-percent').text()).toBe('35%')
    expect(wrapper.find('.progress-ring .scan-eta').exists()).toBe(false)
    expect(wrapper.find('.scan-title .scan-eta').exists()).toBe(true)
    wrapper.unmount()
  })

  it('names the current item through its shared folder, keeping the full path available', async () => {
    const recorded = fixtureBackend({ scenario: 'share_scanning' })
    setBackend(async (method, params) => {
      const answer = await recorded(method, params)
      if (method !== 'jobs.list') return answer
      const list = answer as { jobs: { progress?: { current_subvolume?: object | null } }[] }
      return {
        ...list,
        jobs: list.jobs.map(job => job.progress
          ? { ...job, progress: { ...job.progress, current_subvolume: { path: '/volume1/share04/folder0002/file.bin' } } }
          : job),
      } as never
    })
    const wrapper = mount(App, { attachTo: document.body })
    await flushPromises()
    await vi.dynamicImportSettled()
    await flushPromises()
    const item = wrapper.find('.current-subvolume')
    expect(item.text()).toBe('Current item: share04 › folder0002/file.bin')
    expect(item.attributes('title')).toBe('/volume1/share04/folder0002/file.bin')
    wrapper.unmount()
  })
})
