import type { Job } from '@/types'

/** Recover only one new job for the submitted plan. */
export function findSubmittedJob(list: Job[], kind: Job['kind'], planId: string, knownIds: string[]) {
  const candidates = list.filter(job => {
    if (job.kind !== kind || knownIds.includes(job.job_id)) return false
    if (job.plan_id === planId) return true
    if (kind === 'cleanup-apply' && job.cleanup?.plan_id === planId) return true
    if (kind === 'rootfs-apply') return (job.targets ?? []).some(target => target.path.endsWith(`/${planId}.json`))
    return false
  })
  // Several admins may submit the same selection; never guess which apply to cancel.
  return candidates.length === 1 ? candidates[0] : undefined
}
