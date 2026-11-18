import { flushPromises, mount } from '@vue/test-utils'
import { jobs } from '@/stores/jobs'
import { setBackend } from '@/api'
import type { Backend } from '@/api/methods'
import type { Job } from '@/types'
import RootfsApplyPage from './RootfsApplyPage.vue'

it('keeps following the same rootfs job when it becomes interrupted', async () => {
  const job = { job_id: 'rootfs-follow', kind: 'rootfs-apply', state: 'running', rootfs_plan: { plan_id: 'rootfs-1-2', move_dir: { path: '/volume1/recovered' } } } as Job
  jobs.list = [job]
  setBackend((async () => ({ jobs: jobs.list })) as Backend)
  const wrapper = mount(RootfsApplyPage, { global: { stubs: { RootfsApplyStep: true } } })
  try {
    await flushPromises()
    jobs.list = [{ ...job, state: 'interrupted' }]
    await flushPromises()
    const step = wrapper.getComponent({ name: 'RootfsApplyStep' })
    expect(step.props('jobId')).toBe('rootfs-follow')
    expect(step.props('planId')).toBe('rootfs-1-2')
  } finally { wrapper.unmount() }
})
