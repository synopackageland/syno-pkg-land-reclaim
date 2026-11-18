import { mount } from '@vue/test-utils'
import { jobs } from '@/stores/jobs'
import { nav } from '@/stores/nav'
import { volumes } from '@/stores/volumes'
import type { Job, VolumesDocument } from '@/types'
import StateView from './StateView.vue'

// A volume with no previous result that already has a scan job: the empty state
// reports the scan and never offers to start another one.
const scanJob = (state: Job['state'], mountPath = '/volume1') =>
  ({
    job_id: 'j1', kind: 'scan', state, updated: new Date().toISOString(), filesystems: [{ mount: { path: mountPath } }], targets: [],
    progress: { phase: 'scan', entries: 101982, basis: null },
  }) as unknown as Job

function mountNoReport() {
  return mount(StateView, { props: { kind: 'no_report' }, slots: { default: '<button class="scan-now">Scan now</button>' } })
}

describe('StateView no_report on a volume with a scan job', () => {
  beforeEach(() => {
    volumes.document = { volumes: [{ mount: { path: '/volume1' } }] } as unknown as VolumesDocument
    volumes.selected = '/volume1'
    jobs.list = []
    nav.page = 'overview'
  })

  it.each(['running', 'pausing', 'paused', 'interrupted'] as const)('says the %s scan is there and offers its progress, not a new scan', async state => {
    jobs.list = [scanJob(state)]
    const wrapper = mountNoReport()
    expect(wrapper.find('.scan-now').exists()).toBe(false)
    expect(wrapper.text()).not.toContain('Not scanned yet')
    expect(wrapper.text()).toContain('101,982')
    await wrapper.get('[data-testid="view-scan"]').trigger('click')
    expect(nav.page).toBe('scanning')
  })

  it('names a running scan as scanning and a halted one as paused', () => {
    jobs.list = [scanJob('running')]
    expect(mountNoReport().text()).toContain('Scanning this volume')
    jobs.list = [scanJob('paused')]
    expect(mountNoReport().text()).toContain('Scan paused')
  })

  it('still offers the scan when the job belongs to another volume', () => {
    jobs.list = [scanJob('running', '/volume2')]
    const wrapper = mountNoReport()
    expect(wrapper.find('.scan-now').exists()).toBe(true)
    expect(wrapper.text()).toContain('Not scanned yet')
  })
})
