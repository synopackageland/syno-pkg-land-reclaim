import type { Job } from '@/types'
import { afterEach } from 'vitest'
import { setBackend } from '@/api'
import { fixtureBackend } from '@/api/fixtures'
import { answersVersion } from './answers'
import { activeJobs, jobs, pollInterval, refreshJobs, volumesBehindJobs } from './jobs'
import { volumes } from './volumes'

afterEach(() => {
  jobs.list = []
  volumes.document = null
  setBackend(fixtureBackend())
})

const job = (state: Job['state']) => ({ state }) as Job

describe('pollInterval', () => {
  it('polls every second while a job is pausing or cancelling', () => {
    expect(pollInterval([job('pausing')], false)).toBe(1000)
    expect(pollInterval([job('cancelling')], true)).toBe(1000)
  })

  it('polls every 2 s on the Scanning page', () => {
    expect(pollInterval([job('running')], true)).toBe(2000)
  })

  it('polls every 15 s while a job runs elsewhere, 60 s when idle', () => {
    expect(pollInterval([job('running')], false)).toBe(15000)
    expect(pollInterval([job('completed'), job('paused')], false)).toBe(60000)
  })
})

describe('activeJobs', () => {
  it('lists queued, running, pausing and cancelling jobs', () => {
    const states: Job['state'][] = ['queued', 'running', 'pausing', 'cancelling', 'paused', 'completed', 'failed']
    expect(activeJobs(states.map(job)).map(j => j.state)).toEqual(['queued', 'running', 'pausing', 'cancelling'])
  })
})

describe('refreshJobs', () => {
  it('refreshes volume reports when its first poll finds an already-finished scan', async () => {
    const partial = { kind: 'scan', job_id: 'scan-1', state: 'partial', updated: '2026-10-01T01:05:00Z' } as Job
    const base = fixtureBackend()
    let volumeReads = 0
    jobs.list = []
    volumes.document = null
    setBackend(async (method, params) => {
      if (method === 'jobs.list') return { jobs: [partial] } as never
      if (method === 'volumes.list') {
        volumeReads += 1
        return base(method, params)
      }
      return base(method, params)
    })

    await refreshJobs()

    expect(volumeReads).toBe(1)
    expect(volumes.document).not.toBeNull()
  })

  it('refreshes volume reports when a scan finishes partial', async () => {
    const running = { kind: 'scan', job_id: 'scan-1', state: 'running', updated: '2026-10-01T01:00:00Z' } as Job
    const partial = { ...running, state: 'partial', updated: '2026-10-01T01:05:00Z' } as Job
    const base = fixtureBackend()
    let volumeReads = 0
    jobs.list = [running]
    volumes.document = null
    setBackend(async (method, params) => {
      if (method === 'jobs.list') return { jobs: [partial] } as never
      if (method === 'volumes.list') {
        volumeReads += 1
        return base(method, params)
      }
      return base(method, params)
    })

    await refreshJobs()

    expect(volumeReads).toBe(1)
    expect(volumes.document).not.toBeNull()
  })

  // A scan publish, and a cleanup or rootfs apply that ended or was
  // interrupted, all change what the reports describe.
  it.each([
    ['a scan publishes a report', { kind: 'scan', job_id: 'j1', state: 'completed' } as Job],
    ['a cleanup apply ends', { kind: 'cleanup-apply', job_id: 'j2', state: 'completed' } as Job],
    ['a cleanup apply is interrupted', { kind: 'cleanup-apply', job_id: 'j3', state: 'interrupted' } as Job],
    ['a rootfs apply ends', { kind: 'rootfs-apply', job_id: 'j4', state: 'failed' } as Job],
  ])('invalidates the answers when %s', async (_name, terminal) => {
    const running = { ...terminal, state: 'running' } as Job
    const base = fixtureBackend()
    jobs.list = [running]
    setBackend(async (method, params) => {
      if (method === 'jobs.list') return { jobs: [terminal] } as never
      return base(method, params)
    })
    const before = answersVersion.value

    await refreshJobs()

    expect(answersVersion.value).toBe(before + 1)
  })

  it('keeps the answers of a scan that only paused', async () => {
    const running = { kind: 'scan', job_id: 'scan-1', state: 'running' } as Job
    const paused = { ...running, state: 'paused' } as Job
    const base = fixtureBackend()
    jobs.list = [running]
    setBackend(async (method, params) => {
      if (method === 'jobs.list') return { jobs: [paused] } as never
      return base(method, params)
    })
    const before = answersVersion.value

    await refreshJobs()

    expect(answersVersion.value).toBe(before)
  })
})

// PERF-STANDALONE: the first open read the volumes twice, because every finished
// scan in the job history counted as new to a list that had never been read.
describe('refreshJobs for a caller that reads the volumes alongside', () => {
  const finished = { kind: 'scan', job_id: 'scan-1', state: 'completed', updated: '2026-10-01T01:05:00Z' } as Job

  it('reads no volumes for a scan that finished before the first poll', async () => {
    const base = fixtureBackend()
    let volumeReads = 0
    jobs.list = []
    setBackend(async (method, params) => {
      if (method === 'jobs.list') return { jobs: [finished] } as never
      if (method === 'volumes.list') volumeReads += 1
      return base(method, params)
    })

    await refreshJobs({ reloadVolumes: false })

    expect(volumeReads).toBe(0)
    expect(jobs.list).toEqual([finished])
  })

  it('still invalidates the answers when it finds a report change', async () => {
    jobs.list = []
    setBackend(async () => ({ jobs: [finished] }) as never)
    const before = answersVersion.value

    await refreshJobs({ reloadVolumes: false })

    expect(answersVersion.value).toBe(before + 1)
  })
})

describe('volumesBehindJobs', () => {
  const volumesRead = (generated_at: string) => { volumes.document = { generated_at, volumes: [] } as never }
  const scan = (state: Job['state'], updated: string) => ({ kind: 'scan', job_id: 'scan-1', state, updated }) as Job

  it('is false when the volumes were read after the last finished scan', () => {
    volumesRead('2026-10-01T01:10:00Z')
    jobs.list = [scan('completed', '2026-10-01T01:05:00Z')]
    expect(volumesBehindJobs()).toBe(false)
  })

  it('is true when a scan finished at or after the volumes were read', () => {
    volumesRead('2026-10-01T01:05:00Z')
    jobs.list = [scan('partial', '2026-10-01T01:05:00Z')]
    expect(volumesBehindJobs()).toBe(true)
  })

  it('ignores a scan still running or paused', () => {
    volumesRead('2026-10-01T01:00:00Z')
    jobs.list = [scan('running', '2026-10-01T01:05:00Z'), scan('paused', '2026-10-01T01:06:00Z')]
    expect(volumesBehindJobs()).toBe(false)
  })

  it('is true when the volumes were never read or carry no time', () => {
    jobs.list = [scan('completed', '2026-10-01T01:05:00Z')]
    volumes.document = null
    expect(volumesBehindJobs()).toBe(true)
    volumesRead('')
    expect(volumesBehindJobs()).toBe(true)
  })
})
