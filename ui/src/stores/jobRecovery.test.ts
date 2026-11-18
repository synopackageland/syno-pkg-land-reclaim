import type { Job } from '@/types'
import { findSubmittedJob } from './jobRecovery'

const queued = (id: string, kind: Job['kind'] = 'cleanup-apply', planId = 'p1') => ({
  job_id: id, kind, state: 'queued', plan_id: planId,
}) as Job

describe('findSubmittedJob', () => {
  it('recovers a queued apply by sealed plan id before progress is published', () => {
    expect(findSubmittedJob([queued('j1')], 'cleanup-apply', 'p1', [])?.job_id).toBe('j1')
  })
  it('excludes old jobs, other kinds and other plans', () => {
    expect(findSubmittedJob([queued('old'), queued('root', 'rootfs-apply'), queued('other', 'cleanup-apply', 'p2')], 'cleanup-apply', 'p1', ['old'])).toBeUndefined()
  })
  it('refuses to guess when two new attempts use the same plan', () => {
    expect(findSubmittedJob([queued('j1'), queued('j2')], 'cleanup-apply', 'p1', [])).toBeUndefined()
  })
  it('never attaches a queued cleanup based only on a coincidentally equal target', () => {
    const unknown = { ...queued('j1'), plan_id: undefined, targets: [{ path: '/volume1/share' }] }
    expect(findSubmittedJob([unknown], 'cleanup-apply', 'p1', [])).toBeUndefined()
  })
})
