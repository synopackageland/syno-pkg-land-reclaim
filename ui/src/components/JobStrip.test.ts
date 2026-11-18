import { mount } from '@vue/test-utils'
import { fixtureBackend } from '@/api/fixtures'
import type { Job } from '@/types'
import { nav } from '@/stores/nav'
import JobStrip from './JobStrip.vue'

async function runningJob(): Promise<Job> {
  const { jobs } = await fixtureBackend({ scenario: 'scanning' })('jobs.list', {})
  return jobs[0]!
}

const location = (job: Job) => mount(JobStrip, { props: { job }, attachTo: document.body }).find('[data-ui-list-item-title]').text()

describe('JobStrip', () => {
  afterEach(() => {
    document.body.innerHTML = ''
  })

  it('names a cleanup apply, hides scan controls and offers its progress view', async () => {
    const job = { ...await runningJob(), kind: 'cleanup-apply' as const, phase: 'execute' as const, progress: undefined }
    const wrapper = mount(JobStrip, { props: { job }, attachTo: document.body })
    try {
      expect(wrapper.text()).toContain('Move to Recycle Bin')
      expect(wrapper.text()).not.toContain('Pause')
      expect(wrapper.text()).not.toContain('Cancel scan')
      expect(wrapper.text()).not.toContain('scan.phase.execute')
    } finally { wrapper.unmount() }
  })

  it('shows a running move\'s items done, as it shows again after the window reopens', async () => {
    const cleanup = { plan_id: 'cleanup-1-2', items_total: 50_000, items_done: 12_500, moved: 12_500, already_absent: 0, partial: 0, changed: 0, failed: 0, bytes_total: 1, bytes_moved: 1, calls_done: 12_500, elapsed_s: 160, updated: '2026-11-14T00:00:00Z' }
    const job = { ...await runningJob(), kind: 'cleanup-apply' as const, phase: 'execute' as const, progress: undefined, cleanup }
    const wrapper = mount(JobStrip, { props: { job }, attachTo: document.body })
    try {
      expect(wrapper.text()).toContain('12,500 / 50,000 items')
      expect(wrapper.find('[role="progressbar"]').exists()).toBe(true)
    } finally { wrapper.unmount() }
  })

  it('names the target as the Scanning page does, not its mount path', async () => {
    const job = await runningJob()
    expect(location(job)).toBe('Whole volume1 · Scanning')
  })

  it('names the shares of a share scan', async () => {
    const job = { ...(await runningJob()), scope: null, targets: [{ path: '/volume1/share01' }, { path: '/volume1/share02' }] }
    expect(location(job)).toBe('share01 and 1 more · Scanning')
  })
})

 it('opens the cleanup job by kind and id without offering scan pause', async () => {
  const list = await fixtureBackend({ scenario: 'job_recovery' })('jobs.list', {})
  const job = list.jobs[0]!
  const wrapper = mount(JobStrip, { props: { job }, attachTo: document.body })
  expect(wrapper.text()).toContain('Running')
  expect(wrapper.text()).not.toContain('Pause')
  await wrapper.findAll('button').find(button => button.text() === 'View progress')!.trigger('click')
  expect(nav.page).toBe('cleanupApply')
  expect(nav.followedJobId).toBe(job.job_id)
  wrapper.unmount()
})

it.each(['cleanup-plan', 'rootfs-plan'] as const)('shows %s as planning and never navigates to a scan', async kind => {
  const job = { ...await runningJob(), kind, phase: 'plan' as const, progress: undefined, targets: [] }
  const wrapper = mount(JobStrip, { props: { job }, attachTo: document.body })
  const page = nav.page
  try {
    expect(wrapper.text()).toContain(kind === 'cleanup-plan' ? 'Cleanup planning' : 'System cleanup planning')
    expect(wrapper.text()).not.toContain('Pause')
    await wrapper.findAll('button').find(button => button.text() === 'View progress')!.trigger('click')
    expect(nav.page).toBe(page)
    expect(document.body.textContent).not.toContain(job.job_id)
  } finally { wrapper.unmount() }
})


describe('system partition report row', () => {
  it('names the System partition with its state and follows the job to the shared progress view', async () => {
    const job = { ...await runningJob(), kind: 'rootfs-report' as const, phase: 'report' as const, progress: undefined, elapsed_s: 16 }
    const wrapper = mount(JobStrip, { props: { job }, attachTo: document.body })
    try {
      expect(wrapper.find('[data-ui-list-item-title]').text()).toBe('System partition · Scanning')
      expect(wrapper.text()).toContain('Read system files and check usage')
      expect(wrapper.text()).not.toContain(job.job_id)
      expect(wrapper.findAll('button').map(button => button.text())).toEqual(['View progress'])
      await wrapper.find('button').trigger('click')
      expect(nav.page).toBe('scanning')
      expect(nav.followedJobId).toBe(job.job_id)
    } finally { wrapper.unmount() }
  })

  it('says a queued walk waits to start', async () => {
    const job = { ...await runningJob(), kind: 'rootfs-report' as const, state: 'queued' as const, phase: 'report' as const, progress: undefined }
    const wrapper = mount(JobStrip, { props: { job }, attachTo: document.body })
    try {
      expect(wrapper.text()).toContain('Waiting to start')
    } finally { wrapper.unmount() }
  })
})

it('leaves pausing and cancelling a volume scan to its progress view', async () => {
  const wrapper = mount(JobStrip, { props: { job: await runningJob() }, attachTo: document.body })
  try {
    expect(wrapper.findAll('button').map(button => button.text())).toEqual(['View progress'])
  } finally { wrapper.unmount() }
})
