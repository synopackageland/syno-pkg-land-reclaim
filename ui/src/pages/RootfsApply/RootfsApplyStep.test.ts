import { flushPromises, mount } from '@vue/test-utils'
import { volumes } from '@/stores/volumes'
import shareVolumes from '../../../fixtures/volumes.list/rootfs-cleanup.json'
import { defineComponent } from 'vue'
import { AppRoot, PathDisplay, ToastProvider } from '@synopackageland/ui'
import { ApiError, setBackend } from '@/api'
import type { Backend } from '@/api/methods'
import { answersVersion } from '@/stores/answers'
import { declineRootWork } from '@/authorizationFixture'
import { jobs } from '@/stores/jobs'
import RootfsApplyStep from './RootfsApplyStep.vue'
import {
  rootfsApplyFixtureBackend,
  selectRootfsFixtureScenario,
  type RootfsFixtureScenario,
} from './rootfsApplyFixtureBackend'

/** The step with whatever props a test passes, under the providers the app gives it. */
const Host = defineComponent({
  components: { AppRoot, ToastProvider, RootfsApplyStep },
  inheritAttrs: false,
  template: '<AppRoot><ToastProvider><RootfsApplyStep v-bind="$attrs" /></ToastProvider></AppRoot>',
})

async function mountStep(scenario: RootfsFixtureScenario) {
  document.body.innerHTML = ''
  sessionStorage.clear()
  jobs.list = []
  selectRootfsFixtureScenario(scenario)
  setBackend(rootfsApplyFixtureBackend as unknown as Backend)
  const wrapper = mount(Host, { attachTo: document.body, props: { open: true, planId: 'rootfs-plan-fixture-0001' } })
  await flushPromises()
  return wrapper
}

describe('RootfsApplyStep', () => {
  // File Station opens only DSM's listed shared folders; the app loads them at start.
  beforeEach(() => {
    volumes.document = shareVolumes as never
  })
  afterEach(() => {
    volumes.document = null
  })

  it('shows manual restart and wallpaper follow-ups returned by cleanup without invoking them', async () => {
    selectRootfsFixtureScenario('complete')
    const calls: string[] = []
    setBackend((async (method, params) => {
      calls.push(method)
      const response = await rootfsApplyFixtureBackend(method, params)
      return method === 'rootfs.apply' || method === 'job.result'
        ? { ...response as object, follow_up: ['reset_images', 'restart_winbind', 'update_dsm'] } as never
        : response as never
    }) as Backend)
    const wrapper = mount(Host, { attachTo: document.body, props: { open: true, planId: 'rootfs-plan-fixture-0001' } })
    await vi.waitFor(() => expect(wrapper.text()).toContain('pkg-synosamba-winbindd'))
    expect(wrapper.text()).toContain('Users may need to upload')
    expect(wrapper.text()).toContain('Reclaim will not reboot the NAS')
    expect(calls.every(method => !/restart|reboot/.test(method))).toBe(true)
    wrapper.unmount()
  })

  it.each([
    ['recovered', 'completed'], ['recovered', 'interrupted'],
    ['followed', 'completed'], ['followed', 'interrupted'],
  ] as const)('invalidates answers after the %s rootfs result is read for %s', async (mode, state) => {
    document.body.innerHTML = ''
    sessionStorage.clear()
    jobs.list = []
    jobs.error = null
    selectRootfsFixtureScenario(state === 'completed' ? 'complete' : 'interrupted')
    const calls: string[] = []
    let accepted = mode === 'followed'
    let versionAtResult: number | null = null
    setBackend((async (method, params) => {
      calls.push(method)
      if (method === 'jobs.list') return { jobs: accepted ? [{
        job_id: 'rootfs-job-fixture-0001', kind: 'rootfs-apply', state,
        plan_id: 'rootfs-plan-fixture-0001',
        rootfs_plan: { plan_id: 'rootfs-plan-fixture-0001', move_dir: { path: '/volume1/recovered' } },
      }] : [] } as never
      if (method === 'rootfs.apply') { accepted = true; throw new ApiError('timeout') }
      if (method === 'job.result') versionAtResult = answersVersion.value
      return await rootfsApplyFixtureBackend(method, params) as never
    }) as Backend)
    const wrapper = mount(Host, {
      attachTo: document.body,
      props: { open: true, planId: 'rootfs-plan-fixture-0001', ...(mode === 'followed' ? { jobId: 'rootfs-job-fixture-0001', followOnly: true } : {}) },
    })
    try {
      // The result (or interruption) view is set as soon as job.result
      // resolves; the report and history it reads next can load lazily, so
      // wait for the invalidation itself instead of a fixed number of flushes.
      await vi.waitFor(() => {
        expect(versionAtResult).not.toBeNull()
        expect(wrapper.find('.result-state').exists()).toBe(true)
      })
      // refreshJobs may invalidate first; this proves the result path does too.
      await vi.waitFor(() => expect(answersVersion.value).toBeGreaterThan(versionAtResult!))
      expect(calls.filter(method => method === 'rootfs.apply')).toHaveLength(mode === 'recovered' ? 1 : 0)
      expect(calls.filter(method => method === 'job.result')).toHaveLength(1)
    } finally { wrapper.unmount() }
  })

  it.each(['interrupted', 'failed', 'cancelled', 'missing'])('shows %s without a result, keeps known destination and never polls the result forever', async state => {
    vi.useFakeTimers()
    document.body.innerHTML = ''
    sessionStorage.clear()
    jobs.list = []
    const backend = vi.fn(async (method: string) => {
      if (method === 'rootfs.apply') return { command: 'detach', job_id: 'rootfs-lost' }
      if (method === 'jobs.list') return { jobs: state === 'missing' ? [] : [{
        job_id: 'rootfs-lost', kind: 'rootfs-apply', state, rootfs_plan: { plan_id: 'rootfs-plan-fixture-0001', move_dir: { path: '/volume1/recovered' } },
        rootfs: { phase: 'moves', items_done: 2, items_total: 3 },
      }] }
      if (method === 'job.result') throw new Error('no result')
      return {}
    })
    setBackend(backend as Backend)
    const before = answersVersion.value
    const wrapper = mount(Host, { attachTo: document.body, props: { open: true, planId: 'rootfs-plan-fixture-0001' } })
    try {
      await flushPromises()
      expect(wrapper.text()).toContain('Operation interrupted; some work may have completed')
      // An interrupted or resultless apply may already have moved bytes.
      expect(answersVersion.value).toBeGreaterThan(before)
      if (state !== 'missing') {
        expect(wrapper.findComponent(PathDisplay).props('path')).toBe('/volume1/recovered')
        expect(wrapper.text()).toContain('Moved to: ')
      }
      expect(wrapper.find('[role="progressbar"]').exists()).toBe(false)
      const calls = backend.mock.calls.length
      await vi.advanceTimersByTimeAsync(10000)
      expect(backend).toHaveBeenCalledTimes(calls)
      expect(backend.mock.calls.filter(([method]) => method === 'rootfs.apply')).toHaveLength(1)
      await wrapper.findAll('button').find(button => button.text() === 'Continue cleanup')!.trigger('click')
      await flushPromises()
      expect(backend.mock.calls.filter(([method]) => method === 'rootfs.apply')).toHaveLength(2)
      expect(backend.mock.calls.some(([method]) => method === 'job.resume')).toBe(false)
    } finally {
      wrapper.unmount()
      vi.useRealTimers()
    }
  })

  it('offers recheck after the same plan is refused, without retrying apply automatically', async () => {
    document.body.innerHTML = ''
    sessionStorage.clear()
    jobs.list = []
    let attempts = 0
    const backend = vi.fn(async (method: string, params: unknown) => {
      if (method === 'rootfs.apply') {
        expect(params).toEqual({ plan_id: 'rootfs-plan-fixture-0001', confirm_no_restore: true })
        if (++attempts === 2) return { command: 'rootfs-apply', completion: 'failed', errors: [{ code: 'plan_stale' }] }
        return { command: 'detach', job_id: 'rootfs-lost' }
      }
      if (method === 'jobs.list') return { jobs: [{ job_id: 'rootfs-lost', kind: 'rootfs-apply', state: 'interrupted', rootfs_plan: { plan_id: 'rootfs-plan-fixture-0001', move_dir: { path: '/volume1/recovered' } } }] }
      if (method === 'job.result') throw new Error('no result')
      return {}
    })
    setBackend(backend as Backend)
    const wrapper = mount(Host, { attachTo: document.body, props: { open: true, planId: 'rootfs-plan-fixture-0001' } })
    try {
      await flushPromises()
      await wrapper.findAll('button').find(button => button.text() === 'Continue cleanup')!.trigger('click')
      await flushPromises()
      expect(attempts).toBe(2)
      expect(wrapper.findAll('button').map(button => button.text())).toContain('Check again')
      expect(wrapper.text()).not.toContain('Continue cleanup')
      // The closing row is a DSM footer: its buttons are UI action buttons.
      expect(wrapper.findAll('.actions button').map(button => button.attributes('data-kind'))).toEqual(['action', 'action'])
    } finally { wrapper.unmount() }
  })

  it('shows a never-started job without completed counts or a destructive continue action', async () => {
    const wrapper = await mountStep('not-started')
    try {
      expect(wrapper.text()).toContain('Not started; nothing was done')
      expect(wrapper.text()).not.toContain('Continue cleanup')
      expect(wrapper.text()).not.toContain('Moving files:')
      expect(wrapper.text()).not.toContain('Moved to:')
    } finally { wrapper.unmount() }
  })

  it('does not continue a followed job using an unrelated confirmed plan', async () => {
    document.body.innerHTML = ''
    sessionStorage.clear()
    jobs.list = []
    setBackend((async method => {
      if (method === 'jobs.list') return { jobs: [{ job_id: 'old-rootfs', kind: 'rootfs-apply', state: 'interrupted' }] } as never
      if (method === 'job.result') throw new Error('no result')
      return {} as never
    }) as Backend)
    const wrapper = mount(Host, {
      props: { open: true, jobId: 'old-rootfs', planId: 'different-plan', followOnly: true },
      global: { stubs: { Callout: true, Button: { template: '<button><slot /></button>' } } },
    })
    try {
      await flushPromises()
      expect(wrapper.findAll('button').map(button => button.text())).not.toContain('Continue cleanup')
      expect(wrapper.findAll('button').map(button => button.text())).toContain('Check again')
    } finally { wrapper.unmount() }
  })

  it('finds the queued plan after a lost apply reply without replaying rootfs.apply', async () => {
    sessionStorage.clear()
    jobs.list = []
    jobs.error = null
    selectRootfsFixtureScenario('moves')
    const calls: string[] = []
    setBackend((async (method, params) => {
      calls.push(method)
      const response = await rootfsApplyFixtureBackend(method, params)
      if (method === 'rootfs.apply') throw new ApiError('timeout')
      return response as never
    }) as Backend)
    const wrapper = mount(Host, { attachTo: document.body, props: { open: true, planId: 'rootfs-plan-fixture-0001' } })
    await flushPromises()
    await vi.waitFor(() => expect(wrapper.text()).toContain('Moving files'))
    expect(calls.filter(method => method === 'rootfs.apply')).toHaveLength(1)
    expect(calls.lastIndexOf('jobs.list')).toBeGreaterThan(calls.indexOf('rootfs.apply'))
    wrapper.unmount()
  })


  it('refreshes the report with max_age_s 0 and loads rootfs history after completion', async () => {
    document.body.innerHTML = ''
    sessionStorage.clear()
    jobs.list = []
    selectRootfsFixtureScenario('complete')
    const backend = vi.fn(rootfsApplyFixtureBackend)
    setBackend(backend as unknown as Backend)
    const before = answersVersion.value
    const wrapper = mount(Host, { attachTo: document.body, props: { open: true, planId: 'rootfs-plan-fixture-0001' } })

    await vi.waitFor(() => expect(wrapper.text()).toContain('System partition cleanups'))
    expect(backend.mock.calls).toContainEqual(['rootfs.report', { max_age_s: 0 }])
    expect(backend.mock.calls).toContainEqual(['rootfs.history', {}])
    expect(wrapper.text()).toContain('/volume1/backup/SynoReclaim-system-20260930-1415')
    expect(wrapper.findAll('.result-stats [data-ui-statistic-value]')).toHaveLength(2)
    expect(answersVersion.value).toBeGreaterThan(before)
    wrapper.unmount()
  })

  it('shows item progress and the stop-after-batch action, then follows the paused result', async () => {
    const wrapper = await mountStep('cancellable')
    await vi.waitFor(() => expect(wrapper.text()).toContain('Moving files: 2 of 3'))
    expect(wrapper.text().match(/Moving files: 2 of 3/g) ?? []).toHaveLength(1)
    expect(wrapper.text()).toContain('Stop after this batch')

    await wrapper.findAll('button').find(button => button.text().includes('Stop after this batch'))!.trigger('click')
    await vi.waitFor(() => expect(wrapper.text()).toContain('Stopped. 1 of 3 files moved'))
    expect(wrapper.text()).toContain('Continue')
    wrapper.unmount()
  })

  it('neither stops nor continues the apply when the authorization dialog is dismissed', async () => {
    const calls: string[] = []
    const wrapper = await mountStep('cancellable')
    setBackend((async (method, params) => {
      calls.push(method)
      return await rootfsApplyFixtureBackend(method, params) as never
    }) as Backend)
    const stop = () => wrapper.findAll('button').find(button => button.text().includes('Stop after this batch'))!
    await vi.waitFor(() => expect(wrapper.text()).toContain('Stop after this batch'))
    let undo = declineRootWork()
    try {
      await stop().trigger('click')
      await flushPromises()
      expect(calls).not.toContain('job.cancel')
      expect(stop().attributes('disabled')).toBeUndefined()

      undo()
      await stop().trigger('click')
      await vi.waitFor(() => expect(wrapper.text()).toContain('Continue cleanup'))
      undo = declineRootWork()
      await wrapper.findAll('button').find(button => button.text() === 'Continue cleanup')!.trigger('click')
      await flushPromises()
      expect(calls).not.toContain('rootfs.apply')
      expect(wrapper.text()).toContain('Continue cleanup')
    } finally {
      undo()
      wrapper.unmount()
    }
  })

  it('shows the named DSM tool phase without offering stop', async () => {
    const wrapper = await mountStep('tools')
    await vi.waitFor(() => expect(wrapper.text()).toContain('Running DSM\'s log cleanup: Rotate logs'))
    expect(wrapper.find('.progress-copy').exists()).toBe(false)
    expect(wrapper.text()).not.toContain('Stop after this batch')
    wrapper.unmount()
  })

  it('shows each skipped path grouped by reason and does not report failed cleanup as done', async () => {
    const wrapper = await mountStep('partial')
    await vi.waitFor(() => expect(wrapper.text()).toContain('/var/log/upd@te/update.log'))
    expect(wrapper.text()).toContain('/var/log/upd@te/old.log')
    expect(wrapper.text()).toContain('/var/log/auth.log')
    expect(wrapper.text()).toContain('DSM\'s log cleanup didn\'t finish')
    wrapper.unmount()

    const failed = await mountStep('failed')
    await vi.waitFor(() => expect(failed.text()).toContain('Cleanup did not complete. Review the details below'))
    expect(failed.text()).not.toContain('Done. The system partition has')
    failed.unmount()
  })

  it('opens the move folder in File Station as a shared-folder path', async () => {
    const calls: { id: string; params?: unknown }[] = []
    ;(window as unknown as { SYNO?: unknown }).SYNO = { SDS: { AppLaunch: (id: string, params?: unknown) => calls.push({ id, params }) } }
    const wrapper = await mountStep('complete')
    await vi.waitFor(() => expect(wrapper.find('.location-card').exists()).toBe(true))
    const open = wrapper.find('.location-card button')
    expect(open.attributes('disabled')).toBeUndefined()
    await open.trigger('click')
    expect(calls).toEqual([{ id: 'SYNO.SDS.App.FileStation3.Instance', params: { opendir: '/backup/SynoReclaim-system-20260930-1415' } }])
    delete (window as unknown as { SYNO?: unknown }).SYNO
    wrapper.unmount()
  })

  it('disables Open in File Station for a move folder outside a shared folder', async () => {
    const calls: unknown[] = []
    ;(window as unknown as { SYNO?: unknown }).SYNO = { SDS: { AppLaunch: (...args: unknown[]) => calls.push(args) } }
    selectRootfsFixtureScenario('complete')
    setBackend((async (method, params) => {
      const response = await rootfsApplyFixtureBackend(method, params)
      return method === 'rootfs.apply' || method === 'job.result'
        ? { ...response as object, move_dir: { path: '/volume1/@synoreclaim/moved' } } as never
        : response as never
    }) as Backend)
    document.body.innerHTML = ''
    const wrapper = mount(Host, { attachTo: document.body, props: { open: true, planId: 'rootfs-plan-fixture-0001' } })
    await vi.waitFor(() => expect(wrapper.find('.location-card').text()).toContain('/volume1/@synoreclaim/moved'))
    const open = wrapper.find('.location-card button')
    expect(open.attributes('disabled')).toBeDefined()
    await open.trigger('click')
    expect(calls).toEqual([])
    delete (window as unknown as { SYNO?: unknown }).SYNO
    wrapper.unmount()
  })

  it('drops Check again after a new report says the partition is normal', async () => {
    const wrapper = await mountStep('normal-again')
    await vi.waitFor(() => expect(wrapper.text()).toContain('The system partition is normal again'))
    expect(wrapper.findAll('button').map(button => button.text())).not.toContain('Check again')
    wrapper.unmount()
  })
})
