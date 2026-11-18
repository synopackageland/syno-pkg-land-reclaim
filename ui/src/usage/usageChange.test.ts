import { describe, expect, it } from 'vitest'
import { baselineFor, threshold, sample, dismiss, snooze, WEEK, type Baseline } from './usageChange'
import type { HistoryDocument, Volume } from '@/types'
import volumeDoc from '../../fixtures/volumes.list/volume1.json'
import historyDoc from '../../fixtures/history.list/interaction.json'

const baseline: Baseline = { id: 'scan1', fsid: 'fs1', total: 1e12, used: 400e9 }
const crossing = 450e9
const volume = { ...volumeDoc.volumes[0], fsid: 'fs1', total_bytes: 1e12 } as Volume
const scan = { ...historyDoc.scans[1], fsid: 'fs1', df_total_bytes: 1e12, df_used_bytes: 400e9 } as HistoryDocument['scans'][number]
const first = () => sample(baseline, crossing, undefined, 0)
const alert = () => sample(baseline, crossing, first(), 900000)

describe('usage-change baseline and threshold', () => {
  it('uses decimal 20 GB or 5% of capacity, whichever is greater', () => {
    expect(threshold(100e9)).toBe(20_000_000_000)
    expect(threshold(1e12)).toBe(50e9)
  })
  it('selects the last complete whole-volume scan, never partial or share-only', () => {
    const rows = [
      { ...scan, job_id: 'partial', outcome: 'partial', finished: '2026-10-01T00:00:00Z' },
      { ...scan, job_id: 'share', volume: false },
      { ...scan, job_id: 'old', finished: '2026-09-01T00:00:00Z' },
      scan,
    ] as HistoryDocument['scans']
    expect(baselineFor(volume, rows)?.id).toBe(scan.job_id + ':' + scan.finished)
  })
  it('retains a completed baseline superseded by a partial scan, but rejects legacy unknown outcomes', () => {
    expect(baselineFor(volume, [{ ...scan, outcome: 'superseded', original_outcome: 'completed' }])?.used).toBe(400e9)
    expect(baselineFor(volume, [{ ...scan, outcome: 'superseded' }])).toBeNull()
  })
  it('does not fall back to an older scan when the newest complete baseline is incompatible', () => {
    const old = { ...scan, finished: '2026-09-01T00:00:00Z' }
    for (const changed of [
      { ...scan, fsid: 'replacement' }, { ...scan, fsid: undefined },
      { ...scan, df_total_bytes: 2e12 }, { ...scan, df_used_bytes: null },
    ]) expect(baselineFor(volume, [changed, old])).toBeNull()
    expect(baselineFor({ ...volume, used_bytes: null }, [scan])).toBeNull()
  })
})

describe('usage-change alert state machine', () => {
  it('requires consecutive crossings, including equality and decreases', () => {
    expect(first().visible).toBe(false)
    expect(alert().visible).toBe(true)
    expect(sample(baseline, 350e9, sample(baseline, 350e9, undefined, 0), 900000).visible).toBe(true)
    const below = sample(baseline, crossing - 1, first(), 900000)
    expect(sample(baseline, crossing, below, 1800000).visible).toBe(false)
  })
  it('does not count repeated requests at the same time as another sample', () => {
    expect(sample(baseline, crossing, first(), 0).visible).toBe(false)
  })
  it('hides an alert below the threshold and limits new alerts to seven days', () => {
    const hidden = sample(baseline, 400e9, alert(), 1800000)
    const pending = sample(baseline, crossing, hidden, 2700000)
    expect(sample(baseline, crossing, pending, 3600000).visible).toBe(false)
    expect(sample(baseline, crossing, pending, WEEK + 900000).visible).toBe(true)
  })
  it('snoozes for seven days and only reopens if still crossing', () => {
    const deferred = snooze(alert(), 1000000)
    expect(sample(baseline, crossing, deferred, WEEK).visible).toBe(false)
    expect(sample(baseline, crossing, deferred, WEEK + 1000000).visible).toBe(true)
    expect(sample(baseline, 400e9, deferred, WEEK + 1000000).visible).toBe(false)
  })
  it('dismisses persistently until another threshold change and the rate limit both pass', () => {
    const closed = JSON.parse(JSON.stringify(dismiss(alert(), crossing)))
    expect(sample(baseline, crossing, closed, WEEK * 2).visible).toBe(false)
    const changed = sample(baseline, 500e9, closed, 1800000)
    expect(sample(baseline, 500e9, changed, 2700000).visible).toBe(false)
    expect(sample(baseline, 500e9, changed, WEEK + 900000).visible).toBe(true)
  })
  it('resets all alert records on a new complete scan', () => {
    const next = { ...baseline, id: 'scan2', used: crossing }
    const state = sample(next, crossing, dismiss(alert(), crossing), WEEK)
    expect(state.visible).toBe(false)
    expect(state.lastAlert).toBeNull()
    expect(state.dismissedUsed).toBeNull()
  })
})

it('requires two samples of the new change relative to dismissal, even after seven days', () => {
  const closed = dismiss(alert(), crossing)
  const firstNewChange = sample(baseline, 500e9, closed, WEEK * 2)
  expect(firstNewChange.visible).toBe(false)
  expect(sample(baseline, 500e9, firstNewChange, WEEK * 2 + 900000).visible).toBe(true)
})
