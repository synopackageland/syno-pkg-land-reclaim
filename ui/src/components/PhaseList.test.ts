import { mount } from '@vue/test-utils'
import PhaseList from './PhaseList.vue'
import type { Job } from '@/types'

function job(phase: string) {
  return {
    job_id: 'job-1', kind: 'scan', state: 'running', phase: 'scan', targets: [], scope: null, filesystems: [],
    profile: 'low', created: '2026-09-30T00:00:00Z', updated: '2026-09-30T00:00:00Z', resumes: 0, units: 0,
    reason: null, tool_version: '0.1.0', compatible: true, request: null, request_ignored: null,
    requested_by: null, result: null,
    progress: { phase, units_done: 0, units_total: null, entries: 0, items: 0, bytes_read: 0,
      elapsed_s: 0, rss_bytes: null, subvolume_id: null, current_subvolume: null,
      basis: null, updated: '2026-09-30T00:00:00Z' },
  } as Job
}

describe('PhaseList', () => {
  it('shows only the phases ext4 actually performs', () => {
    const wrapper = mount(PhaseList, { props: { job: job('aggregate'), filesystem: 'ext4' } })
    expect(wrapper.text()).not.toMatch(/subvolumes|snapshots|shared blocks/)
    expect(wrapper.findAll('.phase-row')).toHaveLength(3)
    expect(wrapper.findAll('.phase-row')[1].classes()).toContain('current')
    expect(wrapper.text()).toContain('Count folders and hard links')
  })
  it('folds snapshots into the fourth phase', () => {
    const wrapper = mount(PhaseList, { props: { job: job('snapshots') } })
    expect(wrapper.text()).toContain('Count folders and snapshots')
    expect(wrapper.findAll('.phase-row')[3].classes()).toContain('current')
    expect(wrapper.findAll('.phase-row')).toHaveLength(5)
  })
})
