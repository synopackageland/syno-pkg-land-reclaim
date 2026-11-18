import { currentLocation, isVisiblePausedJob, liveElapsed, middleEllipsis, modeForProfile, progressPhaseIndex, progressRatio } from './scanProgress'
import type { Job } from '@/types'

const progress = {
  phase: 'scan', units_done: 0, units_total: 1, entries: 40, items: 300, bytes_read: 1000,
  elapsed_s: 12, rss_bytes: null, subvolume_id: null, current_subvolume: null,
  basis: { entries: 80, elapsed_s: 24, items: null }, updated: '2026-09-30T00:00:00Z',
} satisfies NonNullable<Job['progress']>

describe('scan progress helpers', () => {
  it('computes progress from entries and ignores the unavailable item basis', () => {
    expect(progressRatio(progress)).toBe(0.5)
  })

  it('uses the metadata items when both runs recorded them', () => {
    expect(progressRatio({ ...progress, items: 300, basis: { entries: 80, elapsed_s: 24, items: 1200 } })).toBe(0.25)
  })

  it('rounds to a whole percent, as the job strip bar prints it', () => {
    expect(progressRatio({ ...progress, items: 227, basis: { entries: 80, elapsed_s: 24, items: 1000 } })).toBe(0.23)
  })

  it('caps an active scan below 100 percent', () => {
    expect(progressRatio({ ...progress, entries: 100 })).toBe(0.99)
  })

  it('has no percentage without an ETA basis', () => {
    expect(progressRatio({ ...progress, basis: null })).toBeNull()
  })

  it('uses counts in snapshot analysis and publishing even with an older volume basis', () => {
    for (const phase of ['snapshots', 'publish']) {
      expect(progressRatio({ ...progress, phase, entries: 1068, basis: { entries: 30_000_000, elapsed_s: 7200, items: null } })).toBeNull()
    }
  })

  it('folds snapshot phase into the aggregation phase', () => {
    expect(progressPhaseIndex('snapshots')).toBe(progressPhaseIndex('aggregate'))
  })

  it('maps the two UI modes to their CLI profiles', () => {
    expect(modeForProfile('fast')).toBe('fast')
    expect(modeForProfile('normal')).toBe('fast')
    expect(modeForProfile('low')).toBe('gentle')
  })

  it('never offers scan resume for an interrupted apply', () => {
    for (const kind of ['cleanup-apply', 'rootfs-apply'] as const) {
      expect(isVisiblePausedJob({ kind, state: 'interrupted', updated: '2026-09-30T00:00:00Z' } as Job, Date.parse('2026-09-30T01:00:00Z'))).toBe(false)
    }
  })

  it('hides paused jobs older than seven days', () => {
    const job = { kind: 'scan', state: 'paused', updated: '2026-09-20T00:00:00Z' } as Job
    expect(isVisiblePausedJob(job, Date.parse('2026-09-30T00:00:00Z'))).toBe(false)
    expect(isVisiblePausedJob({ ...job, updated: '2026-09-25T00:00:00Z' }, Date.parse('2026-09-30T00:00:00Z'))).toBe(true)
  })
})

describe('middleEllipsis', () => {
  it('keeps a short path unchanged', () => {
    expect(middleEllipsis('/volume1/share04', 48)).toBe('/volume1/share04')
  })

  it('keeps both ends of a long path within the limit', () => {
    const long = `/volume1/${'a'.repeat(80)}/leaf.bin`
    const short = middleEllipsis(long, 48)
    expect(short).toHaveLength(48)
    expect(short.startsWith('/volume1/')).toBe(true)
    expect(short.endsWith('leaf.bin')).toBe(true)
    expect(short).toContain('…')
  })
})

describe('currentLocation', () => {
  const shares = [{ path: { path: '/volume1/share04' }, name: { path: 'share04' } }]

  it('names a known shared folder and the path below it', () => {
    expect(currentLocation({ path: '/volume1/share04/folder0002/file.bin' }, shares)).toBe('share04 › folder0002/file.bin')
  })

  it('uses just the folder name at the share root', () => {
    expect(currentLocation({ path: '/volume1/share04' }, shares)).toBe('share04')
  })

  it('never invents a share name for a path outside every share', () => {
    expect(currentLocation({ path: 'name0008' }, shares)).toBe('name0008')
    expect(currentLocation({ path: '/volume1/not-a-share/x' }, shares)).toBe('/volume1/not-a-share/x')
  })

  it('is null without a current item', () => {
    expect(currentLocation(null, shares)).toBeNull()
    expect(currentLocation({ path: '' }, shares)).toBeNull()
  })
})

describe('liveElapsed', () => {
  const job = (state: string, elapsed_s = 100, updated = '2026-09-30T01:00:00Z') =>
    ({ state, progress: { elapsed_s, updated } }) as never
  const poll = { generatedAt: '2026-09-30T01:00:02Z', receivedAt: 1_000_000 }

  it('counts on from the poll: its age at the server, then the time since it arrived', () => {
    expect(liveElapsed(job('running'), poll, 1_000_000)).toBe(102)
    expect(liveElapsed(job('running'), poll, 1_003_400)).toBe(105)
  })

  it('never counts a server clock behind the progress as negative age', () => {
    expect(liveElapsed(job('running', 100, '2026-09-30T02:00:00Z'), poll, 1_001_000)).toBe(101)
  })

  it('holds the last value while the scan is not running', () => {
    for (const state of ['paused', 'interrupted', 'completed', 'pausing'])
      expect(liveElapsed(job(state), poll, 1_009_000), state).toBe(100)
  })

  it('shows the polled value without a poll time, and nothing without progress', () => {
    expect(liveElapsed(job('running'), null, 1_009_000)).toBe(100)
    expect(liveElapsed({ state: 'running' } as never, poll, 1_009_000)).toBeUndefined()
  })
})
