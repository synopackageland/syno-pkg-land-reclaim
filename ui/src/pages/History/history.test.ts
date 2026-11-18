import { describe, expect, it } from 'vitest'
import { catalogs } from '@/i18n/catalogs'
import { createTranslator } from '@/i18n/t'
import { coversVolume, historyRows, outcomeTone, trendAxisLabels, trendPoints, trendSubtitle } from './history'
import type { HistoryDocument, Job } from '@/types'

const scan = (over: Partial<HistoryDocument['scans'][number]> = {}): HistoryDocument['scans'][number] => ({
  job_id: 'job-finished',
  targets: [{ path: '/volume1' }],
  volume: true,
  profile: 'fast',
  started: '2026-09-20T08:00:00Z',
  finished: '2026-09-20T08:11:00Z',
  elapsed_ms: 660000,
  total_elapsed_ms: 660000,
  entries: 4800000,
  subvolumes: 13,
  reused_units: 0,
  outcome: 'completed',
  spill_peak_bytes: 1,
  report: { path: '/volume1/@appdata/SynoReclaim/state/reports/x.srrep' },
  report_bytes: 1,
  df_total_bytes: 100,
  df_used_bytes: 40,
  df_free_bytes: 60,
  data_used_bytes: null,
  ...over,
})

const job = (over: Partial<Job> = {}): Job =>
  ({
    kind: 'scan',
    job_id: 'job-live',
    state: 'paused',
    phase: 'scan',
    targets: [{ path: '/volume1' }],
    scope: 'volume',
    filesystems: [],
    profile: 'low',
    created: '2026-09-30T01:00:00Z',
    updated: '2026-09-30T01:04:00Z',
    resumes: 0,
    units: 1,
    reason: 'paused',
    tool_version: '0.1.0',
    compatible: true,
    request: null,
    request_ignored: null,
    requested_by: 'admin',
    result: null,
    ...over,
  }) as Job

const history = (scans: HistoryDocument['scans']): HistoryDocument =>
  ({
    schema_version: 1,
    command: 'history',
    tool_version: '0.1.0',
    generated_at: '2026-09-30T00:00:00Z',
    completion: 'complete',
    job_id: null,
    errors: [],
    warnings: [],
    scans,
  }) as HistoryDocument

describe('historyRows', () => {
  it('puts an unfinished job above the finished scans, newest first', () => {
    const rows = historyRows(history([scan()]), [job()])
    expect(rows.map(r => r.id)).toEqual(['job-live', 'job-finished'])
    expect(rows[0].live).toBe(true)
    expect(rows[1].live).toBe(false)
  })

  it('preserves apply kinds without inventing scan statistics or report actions', () => {
    for (const kind of ['cleanup-apply', 'rootfs-apply'] as const) {
      const [row] = historyRows(history([]), [job({ kind, state: 'interrupted' })])
      expect(row.kind).toBe(kind)
      expect(row.entries).toBeNull()
      expect(row.hasReport).toBe(false)
      expect(row.missingResult).toBe(true)
    }
  })

  it('does not list a finished job twice', () => {
    const rows = historyRows(history([scan({ job_id: 'job-live' })]), [job({ state: 'paused' })])
    expect(rows).toHaveLength(1)
    expect(rows[0].live).toBe(true)
  })

  it('ignores finished jobs, because history already records them', () => {
    const rows = historyRows(history([]), [job({ state: 'completed' })])
    expect(rows).toEqual([])
  })

  it('reads live progress and keeps a missing peak RSS unknown', () => {
    const [row] = historyRows(history([]), [
      job({ progress: { elapsed_s: 250, entries: 1250, rss_bytes: null } as Job['progress'] }),
    ])
    expect(row.elapsedMs).toBe(250000)
    expect(row.entries).toBe(1250)
    expect(row.peakRssBytes).toBeNull()
  })

  it('marks a scan with a published report as viewable', () => {
    const [withReport] = historyRows(history([scan()]), [])
    const [without] = historyRows(history([scan({ report: undefined })]), [])
    expect(withReport.hasReport).toBe(true)
    expect(without.hasReport).toBe(false)
  })
})

describe('trendPoints', () => {
  it('keeps only scans that cover the mount, oldest first, with a used figure', () => {
    const points = trendPoints(
      history([
        scan({ job_id: 'b', df_used_bytes: 40, finished: '2026-09-20T08:00:00Z' }),
        scan({ job_id: 'a', df_used_bytes: 30, finished: '2026-09-10T08:00:00Z' }),
        scan({ job_id: 'c', df_used_bytes: null }),
        scan({ job_id: 'd', targets: [{ path: '/volume2' }], df_used_bytes: 99 }),
      ]),
      '/volume1',
    )
    expect(points.map(p => p.value)).toEqual([30, 40])
  })

  it('keeps one point for scans that finished at the same time', () => {
    const at = '2026-09-29T11:54:34Z'
    const points = trendPoints(history([scan({ job_id: 'a', finished: at }), scan({ job_id: 'b', finished: at })]), '/volume1')
    expect(points).toEqual([{ at, value: 40 }])
  })

  it('counts a path scan inside the volume as the volume', () => {
    const points = trendPoints(history([scan({ volume: false, targets: [{ path: '/volume1/video' }] })]), '/volume1')
    expect(points).toHaveLength(1)
  })
})

describe('coversVolume', () => {
  it('matches the mount itself and paths below it', () => {
    expect(coversVolume({ targets: [{ path: '/volume1' }] }, '/volume1')).toBe(true)
    expect(coversVolume({ targets: [{ path: '/volume1/video' }] }, '/volume1')).toBe(true)
    expect(coversVolume({ targets: [{ path: '/volume10' }] }, '/volume1')).toBe(false)
    expect(coversVolume({ targets: [{ path: '/volume2' }] }, '/volume1')).toBe(false)
  })
})

describe('outcomeTone', () => {
  it('maps the states to badge tones and unknown values to neutral', () => {
    expect(outcomeTone('completed')).toBe('success')
    expect(outcomeTone('partial')).toBe('warning')
    expect(outcomeTone('failed')).toBe('danger')
    expect(outcomeTone('paused')).toBe('info')
    expect(outcomeTone('superseded')).toBe('neutral')
    expect(outcomeTone('weird')).toBe('neutral')
  })
})

describe('trendAxisLabels', () => {
  const at = (iso: string) => Date.parse(iso)
  const utc = { locale: 'en-GB', timeZone: 'UTC' }

  it('labels a trend across days with dates', () => {
    expect(trendAxisLabels(at('2026-09-26T11:54:00Z'), 3 * 86_400_000, 4, utc)).toEqual(['26/09', '27/09', '28/09', '29/09'])
  })

  it('labels scans of one day with the time of day', () => {
    expect(trendAxisLabels(at('2026-09-30T01:13:00Z'), 6 * 60_000, 3, utc)).toEqual(['01:13', '01:16', '01:19'])
  })

  it('adds seconds when minutes would repeat', () => {
    expect(trendAxisLabels(at('2026-09-30T01:13:00Z'), 60_000, 3, utc)).toEqual(['01:13:00', '01:13:30', '01:14:00'])
  })

  it('adds the time when dates would repeat', () => {
    expect(trendAxisLabels(at('2026-09-29T12:00:00Z'), 86_400_000, 3, utc)).toEqual(['29/09, 12:00', '30/09, 00:00', '30/09, 12:00'])
  })
})

describe('trendSubtitle', () => {
  const MINUTE = 60_000
  const { t: en } = createTranslator('en', catalogs)
  const { t: zh } = createTranslator('zh-TW', catalogs)

  it('gives a span under an hour in minutes, at least one', () => {
    expect(trendSubtitle('43.3 GB', '43.3 GB', 23 * MINUTE, en)).toBe('43.3 GB → 43.3 GB over 23 minutes')
    expect(trendSubtitle('1 GB', '2 GB', 20_000, en)).toBe('1 GB → 2 GB over 1 minute')
  })

  it('gives a span under a day in hours', () => {
    expect(trendSubtitle('1 GB', '2 GB', 60 * MINUTE, en)).toBe('1 GB → 2 GB over 1 hour')
    expect(trendSubtitle('1 GB', '2 GB', 5.4 * 60 * MINUTE, en)).toBe('1 GB → 2 GB over 5 hours')
  })

  it('gives a longer span in days', () => {
    expect(trendSubtitle('1 GB', '2 GB', 24 * 60 * MINUTE, en)).toBe('1 GB → 2 GB over 1 day')
    expect(trendSubtitle('1 GB', '2 GB', 3 * 24 * 60 * MINUTE, en)).toBe('1 GB → 2 GB over 3 days')
  })

  it('moves to the next unit when rounding reaches it', () => {
    expect(trendSubtitle('1 GB', '2 GB', 59.6 * MINUTE, en)).toBe('1 GB → 2 GB over 1 hour')
    expect(trendSubtitle('1 GB', '2 GB', 23.7 * 60 * MINUTE, en)).toBe('1 GB → 2 GB over 1 day')
  })

  it('reads in zh-TW', () => {
    expect(trendSubtitle('43.3 GB', '43.3 GB', 23 * MINUTE, zh)).toBe('23 分鐘內從 43.3 GB 到 43.3 GB')
    expect(trendSubtitle('1 GB', '2 GB', 5 * 60 * MINUTE, zh)).toBe('5 小時內從 1 GB 到 2 GB')
    expect(trendSubtitle('1 GB', '2 GB', 3 * 24 * 60 * MINUTE, zh)).toBe('3 天內從 1 GB 到 2 GB')
  })
})
