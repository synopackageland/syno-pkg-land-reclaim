// Job states of jobs.list grouped the way the UI reads them.
import type { Job } from '@/types'

/** A job that runs or is about to: it holds the volume. */
export const ACTIVE_STATES: readonly Job['state'][] = ['queued', 'running', 'pausing', 'cancelling']
/** A job that stopped before it finished; only a scan can use resume. */
export const HALTED_STATES: readonly Job['state'][] = ['paused', 'interrupted']

export const isActiveJob = (job: Job) => ACTIVE_STATES.includes(job.state)
export const isHaltedJob = (job: Job) => HALTED_STATES.includes(job.state)
