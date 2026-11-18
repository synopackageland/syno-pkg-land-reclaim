import { describe, expect, it } from 'vitest'
import { heldByOfRoot, nextMetricTargets, scannedShares, SHARE_METRIC_PAGE } from './shareReports'
import type { HistoryDocument, TopDocument } from '@/types'

type Scan = HistoryDocument['scans'][number]

const scan = (over: Partial<Scan>): Scan => ({
  job_id: 'job',
  targets: [{ path: '/volume1/share01' }],
  volume: false,
  started: '2026-09-30T08:00:00Z',
  finished: '2026-09-30T08:01:00Z',
  elapsed_ms: 60000,
  total_elapsed_ms: 60000,
  entries: 10,
  subvolumes: 1,
  reused_units: 0,
  outcome: 'completed',
  spill_peak_bytes: 1,
  report: { path: '/state/reports/x.srrep' },
  report_bytes: 1,
  df_total_bytes: 100,
  df_used_bytes: 40,
  df_free_bytes: 60,
  data_used_bytes: null,
  ...over,
})

const history = (scans: Scan[]) => ({ scans }) as HistoryDocument

describe('scannedShares', () => {
  it('lists the shares of the volume that a completed share scan left a report for, once each', () => {
    const shares = scannedShares(history([
      scan({ targets: [{ path: '/volume1/share01' }, { path: '/volume1/share02', path_hex: '2f766f6c756d65312f7368617265303' }] }),
      scan({ targets: [{ path: '/volume1/share01' }] }),
    ]), '/volume1')
    expect(shares).toEqual([{ path: '/volume1/share01' }, { path: '/volume1/share02', path_hex: '2f766f6c756d65312f7368617265303' }])
  })

  it('leaves out volume scans, scans without a report, unfinished scans, other volumes and paths below a share', () => {
    const shares = scannedShares(history([
      scan({ targets: [{ path: '/volume1' }], volume: true }),
      scan({ targets: [{ path: '/volume1/share01' }], report: undefined }),
      scan({ targets: [{ path: '/volume1/share02' }], outcome: 'partial' }),
      scan({ targets: [{ path: '/volume2/share03' }] }),
      scan({ targets: [{ path: '/volume1/share04/folder' }] }),
    ]), '/volume1')
    expect(shares).toEqual([])
  })
})

describe('nextMetricTargets', () => {
  it('returns the untried paths, one page at a time', () => {
    const paths = ['a', 'b', 'c']
    expect(nextMetricTargets(paths, path => path === 'a', 1)).toEqual(['b'])
    expect(nextMetricTargets(paths, () => true)).toEqual([])
  })

  it('never asks for more than one page', () => {
    const paths = Array.from({ length: SHARE_METRIC_PAGE + 10 }, (_, index) => `share${index}`)
    expect(nextMetricTargets(paths, () => false)).toHaveLength(SHARE_METRIC_PAGE)
  })
})

type Root = TopDocument['results'][number]['target']

const root = (metrics: Partial<Root['metrics']>, blockers: Root['blockers']): Root => ({
  path: '/volume1/share01',
  kind: 'dir',
  flags: ['subvolume_root'],
  metrics: {
    logical: 100, physical: 100, shared: 0, reclaimable: 100, reclaimable_resolved: 100, unknown: 0,
    pinned_unreferenced: 0, extents: 1, inline: 0, entries: 1, files: 1, ...metrics,
  },
  blockers,
  nlink: 1,
  tree: 256,
  objectid: 256,
})

describe('heldByOfRoot', () => {
  it('holds nothing when the whole root can be freed', () => {
    expect(heldByOfRoot(root({}, []))).toEqual([])
  })

  it('gives the bytes not freed to the one holder its blockers name', () => {
    expect(heldByOfRoot(root({ reclaimable: 30 }, ['snapshot', 'shared_tree']))).toEqual([{ class: 'snapshot', bytes: 70 }])
    expect(heldByOfRoot(root({ reclaimable: 30 }, ['live_reflink']))).toEqual([{ class: 'reflink', bytes: 70 }])
  })

  it('keeps unknown bytes apart from the holder, largest first', () => {
    expect(heldByOfRoot(root({ reclaimable: 10, unknown: 60 }, ['hard_link', 'unknown']))).toEqual([
      { class: 'unknown', bytes: 60 },
      { class: 'hard_link', bytes: 30 },
    ])
  })

  it('cannot split the bytes not freed between several holders', () => {
    expect(heldByOfRoot(root({ reclaimable: 30 }, ['snapshot', 'hard_link']))).toBeNull()
  })

  it('does not know the holders of a root with no reclaimable value', () => {
    expect(heldByOfRoot(root({ reclaimable: null }, ['snapshot']))).toBeNull()
  })
})
