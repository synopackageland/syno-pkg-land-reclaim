import { findEntry, fixtureBackend, type FixtureEntry } from './fixtures'

const entries: FixtureEntry[] = [
  { file: 'volume1.json', params: { volume: '/volume1' } },
  { file: 'volume2.json', params: { volume: '/volume2' } },
  { file: 'volume1-partial.json', params: { volume: '/volume1' }, scenario: 'partial' },
  { scenario: 'busy', document: { completion: 'busy', exit: 4 } },
]

describe('findEntry', () => {
  it('matches the listed params', () => {
    expect(findEntry(entries, { volume: '/volume2' }, 'normal')?.file).toBe('volume2.json')
  })

  it('prefers the scenario entry, then falls back to normal', () => {
    expect(findEntry(entries, { volume: '/volume1' }, 'partial')?.file).toBe('volume1-partial.json')
    expect(findEntry(entries, { volume: '/volume2' }, 'partial')?.file).toBe('volume2.json')
  })

  it('matches an entry without params for any call', () => {
    expect(findEntry(entries, { volume: '/volume9' }, 'busy')?.document).toEqual({ completion: 'busy', exit: 4 })
  })

  it('compares params deeply', () => {
    const list = [{ file: 'a.json', params: { paths: ['/volume1/a'] } }]
    expect(findEntry(list, { paths: ['/volume1/a'] }, 'normal')?.file).toBe('a.json')
    expect(findEntry(list, { paths: ['/volume1/b'] }, 'normal')).toBeUndefined()
  })
})

describe('fixtureBackend', () => {
  it('serves the seed volumes.list fixture', async () => {
    const doc = await fixtureBackend()('volumes.list', {})
    expect(doc.command).toBe('volumes')
  })

  it('keeps the cleanup kill switch the dev page last set, as a stateful backend would', async () => {
    const backend = fixtureBackend()
    expect(await backend('setup.cleanup', { mutations_disabled: true })).toMatchObject({ enables_cleanup: false, config: { cleanup_enabled: true, mutations_disabled: true } })
    expect(await backend('session.get', {})).toMatchObject({ cleanup_enabled: true, mutations_disabled: true })
    expect(await backend('setup.cleanup', { mutations_disabled: false })).toMatchObject({ enables_cleanup: true, config: { cleanup_enabled: true, mutations_disabled: false } })
    expect(await backend('session.get', {})).toMatchObject({ mutations_disabled: false })
  })

  it('rejects with fixture_missing when nothing matches', async () => {
    await expect(fixtureBackend()('top.list', { target: '/nowhere', kind: 'all' })).rejects.toMatchObject({ code: 'fixture_missing' })
  })

  it('rejects not_admin sessions with no_permission', async () => {
    await expect(fixtureBackend({ scenario: 'not_admin' })('session.get', {})).rejects.toMatchObject({ code: 'no_permission' })
  })

  it('maps invalid parameter refusals to invalid_param', async () => {
    await expect(fixtureBackend({ scenario: 'invalid_param' })('top.list', { target: 'relative', kind: 'all' }))
      .rejects.toMatchObject({ code: 'invalid_param', exit: 2 })
  })

  it('serves a cleanup plan and terminal apply job for the review preview', async () => {
    const backend = fixtureBackend({ scenario: 'review_cleanup' })
    const accepted = await backend('cleanup.plan', { paths: [{ path: '/volume1/share01/cleanup-golden/apply' }] })
    const plan = await backend('job.result', { job_id: accepted.job_id! }) as unknown as { plan_id: string; shares: { recycle_bin: { enabled: boolean } }[] }
    expect(plan.plan_id).toBe('cleanup-18d9cccbc2d8b089-31e7a5')
    expect(plan.shares[0]?.recycle_bin.enabled).toBe(true)

    const started = await backend('cleanup.apply', { plan_id: 'cleanup-18d9cccbc2d8b089-31e7a5', confirm_move: true })
    expect(started.command).toBe('detach')
    expect(started.job_id).toBe('cleanup-18d9cccbc2d8b089-12701')
    if (typeof started.job_id !== 'string') throw new Error('cleanup.apply did not return a queued job id')

    const jobs = await backend('jobs.list', {})
    expect(jobs.jobs[0].job_id).toBe(started.job_id)
    expect((jobs.jobs[0] as unknown as { kind: string }).kind).toBe('cleanup-apply')
    expect((jobs.jobs[0] as unknown as { phase: string }).phase).toBe('execute')
    expect((jobs.jobs[0] as unknown as { cleanup: { bytes_moved: number } }).cleanup.bytes_moved).toBe(1048576)

    const result = await backend('job.result', { job_id: started.job_id })
    expect(result.command).toBe('cleanup-apply')
    expect(result.completion).toBe('complete')
    expect((result as { report_state_stale?: boolean }).report_state_stale).toBe(true)
  })

  it('serves a plan with a shared folder whose Recycle Bin is off', async () => {
    const backend = fixtureBackend({ scenario: 'review_recycle_off' })
    const accepted = await backend('cleanup.plan', { paths: [] })
    const plan = await backend('job.result', { job_id: accepted.job_id! }) as unknown as { plan_id: string; shares: { recycle_bin: { enabled: boolean } }[] }
    expect(plan.shares.map(share => share.recycle_bin.enabled)).toEqual([true, false])
  })

  it('serves a long refusal list for the review layout preview', async () => {
    const backend = fixtureBackend({ scenario: 'review_long_refusal' })
    const accepted = await backend('cleanup.plan', { paths: [] })
    const plan = (await backend('job.result', { job_id: accepted.job_id! })) as unknown as {
      completion: string
      errors: { code: string; path: string }[]
    }
    expect(plan.completion).toBe('failed')
    expect(plan.errors).toHaveLength(40)
    expect(plan.errors[0]?.code).toBe('target_changed')
  })

  it('returns a refusal document from job.result when a queued cleanup job fails to start', async () => {
    const backend = fixtureBackend({ scenario: 'review_cleanup_refused' })
    const started = await backend('cleanup.apply', { plan_id: 'cleanup-18d9cccbc2d8b089-31e7a5', confirm_move: true })
    if (typeof started.job_id !== 'string') throw new Error('cleanup.apply did not return a queued job id')
    const jobs = await backend('jobs.list', {})
    expect(jobs.jobs[0].job_id).toBe(started.job_id)
    expect(jobs.jobs[0].state).toBe('failed')

    const result = await backend('job.result', { job_id: started.job_id })
    expect(result.command).toBe('cleanup-apply')
    expect(result.completion).toBe('failed')
    expect(result.errors[0].code).toBe('plan_stale')
  })

  it('serves the many-volumes scenario with eight volumes and three hundred shares', async () => {
    const backend = fixtureBackend({ scenario: 'many_volumes' })
    const doc = await backend('volumes.list', {})
    expect(doc.volumes).toHaveLength(8)
    expect(doc.volumes[0].shares).toHaveLength(300)
  })

  it('answers the share_scan scenario from one share scan everywhere', async () => {
    const backend = fixtureBackend({ scenario: 'share_scan' })
    const volume = (await backend('volumes.list', {})).volumes[0]
    const history = await backend('history.list', {})
    const scan = history.scans.find(row => row.outcome === 'completed' && row.targets[0]?.path === '/volume1/share04')
    const jobs = (await backend('jobs.list', {})).jobs
    const reports = [
      (await backend('top.list', { target: '/volume1/share04', kind: 'all' })).results[0]?.report,
      (await backend('top.list', { target: '/volume1/share04', kind: 'files' })).results[0]?.report,
      (await backend('recycle.list', { target: '/volume1/share04' })).report_info,
    ]

    expect(jobs.map(job => [job.job_id, job.state])).toEqual([[scan?.job_id, 'completed']])
    for (const report of reports) {
      expect(report?.job_id).toBe(scan?.job_id)
      expect(report?.scan_finished).toBe(scan?.finished)
      expect(report?.consistency).toBe(volume.last_consistency)
    }
    expect(volume.last_scan).toBe(scan?.finished)
  })
})
