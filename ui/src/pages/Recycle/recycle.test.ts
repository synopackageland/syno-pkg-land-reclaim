import { canLoadMore, nextOldestLimit, OLDEST_MAX, OLDEST_PAGE, olderThan, shareAnswersAsVolume, shareTarget, sortOldest, type OldestItem } from './recycle'
import type { RecycleDocument } from '@/types'

const item = (path: string, ctime: string | null, bytes = 1): OldestItem => ({
  path: { path },
  ctime,
  bytes,
  reclaimable_bytes: bytes,
})

const NOW = Date.parse('2026-09-30T12:00:00Z')

describe('shareTarget', () => {
  it('joins the volume-relative share name onto the mount', () => {
    expect(shareTarget('/volume1', { path: 'share01' })).toBe('/volume1/share01')
    expect(shareTarget('/volume1/', { path: 'homes/alice' })).toBe('/volume1/homes/alice')
  })

  it('keeps an absolute share path', () => {
    expect(shareTarget('/volume1', { path: '/volume1/share01' })).toBe('/volume1/share01')
  })
})

describe('oldest limits', () => {
  it('starts at a page of 200 and loads up to 1000', () => {
    expect(OLDEST_PAGE).toBe(200)
    expect(nextOldestLimit(OLDEST_PAGE)).toBe(OLDEST_MAX)
    expect(nextOldestLimit(OLDEST_MAX)).toBe(OLDEST_MAX)
  })

  it('has more to load while items are missing and the limit can grow', () => {
    expect(canLoadMore(8412, 200, OLDEST_PAGE)).toBe(true)
    expect(canLoadMore(250, 250, OLDEST_MAX)).toBe(false)
    expect(canLoadMore(8412, 1000, OLDEST_MAX)).toBe(false)
  })
})

describe('sortOldest', () => {
  it('sorts oldest first and items without a ctime last', () => {
    const unsorted = [item('/b', '2026-02-17T00:00:00Z'), item('/c', null), item('/a', '2024-03-14T00:00:00Z')]
    expect(sortOldest(unsorted).map(i => i.path.path)).toEqual(['/a', '/b', '/c'])
  })
})

describe('olderThan', () => {
  const items = [item('/old', '2024-03-14T00:00:00Z'), item('/mid', '2026-09-15T00:00:00Z'), item('/none', null)]

  it('selects the items moved more than N days ago, never one without a ctime', () => {
    expect(olderThan(items, 30, NOW).map(i => i.path.path)).toEqual(['/old'])
    expect(olderThan(items, 7, NOW).map(i => i.path.path)).toEqual(['/old', '/mid'])
    expect(olderThan(items, 1000, NOW)).toEqual([])
  })
})

describe('shareAnswersAsVolume', () => {
  const answer = (share: string, over: Partial<RecycleDocument> = {}) => ({
    completion: 'complete',
    errors: [],
    warnings: [{ code: 'recycle_age_unavailable', message: 'x' }],
    report_info: { scan_finished: `2026-09-30T0${share.length}:00:00Z` },
    shares: [{ share: { path: share }, items: 1, bytes: 1, reclaimable_bytes: 0, held_by: [] }],
    ...over,
  }) as unknown as RecycleDocument
  const volume = answer('', { completion: 'failed', errors: [{ code: 'no_report', message: 'x' }] as never, report_info: null, shares: [] })

  it('joins the share answers that have a report into one volume answer', () => {
    const joined = shareAnswersAsVolume(volume, [answer('share01'), null, answer('share02', { completion: 'failed', report_info: null, shares: [] })])
    expect(joined.completion).toBe('complete')
    expect(joined.errors).toEqual([])
    expect(joined.shares.map(share => share.share.path)).toEqual(['share01'])
    expect(joined.report_info).toEqual(answer('share01').report_info)
    expect(joined.warnings.map(warning => warning.code)).toEqual(['recycle_age_unavailable'])
  })

  it('describes the oldest share report, as Explore does', () => {
    const newer = answer('share01', { report_info: { scan_finished: '2026-09-30T09:00:00Z' } as never })
    const older = answer('share02', { report_info: { scan_finished: '2026-09-29T09:00:00Z' } as never })
    expect(shareAnswersAsVolume(volume, [newer, older]).report_info).toEqual(older.report_info)
  })

  it('has no report and no shares when no share answer has one', () => {
    const joined = shareAnswersAsVolume(volume, [null])
    expect(joined.report_info).toBeNull()
    expect(joined.shares).toEqual([])
  })
})
