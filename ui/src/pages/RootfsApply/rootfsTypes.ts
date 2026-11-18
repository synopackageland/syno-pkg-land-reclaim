import type { Job, RootfsApplyDocument, RootfsHistoryDocument } from '@/types'

export type RootfsApplyProgress = NonNullable<Job['rootfs']>
export type RootfsApplyJob = Pick<Job, 'job_id' | 'kind' | 'state' | 'rootfs' | 'rootfs_plan'> & { reason?: string | null } & { plan_id?: string }
export type RootfsApplyResult = RootfsApplyDocument
export type RootfsHistoryRecord = RootfsHistoryDocument['applies'][number]
