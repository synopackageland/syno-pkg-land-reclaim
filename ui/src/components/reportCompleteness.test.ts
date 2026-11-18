import { describe, expect, it } from 'vitest'
import { reportCompleteness, visibleConsistency } from './reportCompleteness'

const GiB = 2 ** 30
const evidence = (unknown_bytes: number, used_bytes: number | null, coverage_gaps = false) => ({ unknown_bytes, used_bytes, coverage_gaps })

describe('report completeness presentation', () => {
  it('shows completed live scans as approximate even with large namespace gaps', () => {
    const attribution = { ...evidence(5 * GiB, 100 * GiB, true), approximation: { changed_subvolumes: 5, skipped_roots: 2, unaccounted_bytes: 5 * GiB } }
    expect(reportCompleteness({ consistency: 'partial', attribution }, 'complete')).toMatchObject({ warn: false, unknownNote: true })
    expect(visibleConsistency({ consistency: 'partial', attribution })).toBe('approximate')
    expect(reportCompleteness({ consistency: 'partial', attribution }, 'complete', true).warn).toBe(true)
    expect(reportCompleteness({ consistency: 'partial', attribution }, 'cancelled').warn).toBe(true)
  })
  it('includes exactly 1 GiB and exactly 3% in the small bucket', () => {
    expect(reportCompleteness({ attribution: evidence(GiB, GiB * 100), consistency: 'partial' }).smallUnknown).toBe(true)
    expect(reportCompleteness({ attribution: evidence(30, 1000), consistency: 'partial' })).toMatchObject({ warn: false, smallUnknown: true, unknownNote: false })
    expect(visibleConsistency({ attribution: evidence(30, 1000), consistency: 'partial' })).toBe('small_unknown')
  })
  it('moves 3% + 1 byte and 1 GiB + 1 byte to the muted note, not the banner', () => {
    for (const a of [evidence(31, 1000), evidence(GiB + 1, (GiB + 1) * 100), evidence(GiB + 1, 2 ** 40)]) {
      expect(reportCompleteness({ attribution: a, consistency: 'partial' })).toMatchObject({ warn: false, smallUnknown: false, unknownNote: true })
      expect(visibleConsistency({ attribution: a, consistency: 'partial' })).toBe('unattributed')
    }
  })
  it('always warns about coverage gaps, even with zero unknown bytes', () => {
    expect(reportCompleteness({ attribution: evidence(0, 1000, true), consistency: 'partial' })).toMatchObject({ warn: true, unknownNote: false })
    expect(reportCompleteness({ attribution: evidence(5000, 1000, true), consistency: 'partial' })).toMatchObject({ warn: true, unknownNote: false })
  })
  it('keeps failed, aborted, uncertain and legacy reports visible', () => {
    for (const completion of ['failed', 'cancelled', 'paused', 'runtime_limit', 'resource_limit']) {
      expect(reportCompleteness({ attribution: evidence(1, 1000) }, completion).warn).toBe(true)
      expect(reportCompleteness({ attribution: evidence(900, 1000) }, completion)).toMatchObject({ warn: true, unknownNote: false })
    }
    expect(reportCompleteness({ consistency: 'partial' }).warn).toBe(true)
    expect(reportCompleteness({ attribution: evidence(1, 0), consistency: 'partial' }).warn).toBe(true)
    expect(reportCompleteness({ attribution: evidence(1, null), consistency: 'partial' }).warn).toBe(true)
  })
  it('preserves changed-during-scan and checks per-answer errors independently', () => {
    expect(visibleConsistency({ attribution: evidence(1, 1000), consistency: 'changed-during-scan' })).toBe('changed-during-scan')
    expect(reportCompleteness({ attribution: evidence(1, 1000), consistency: 'partial' }, 'partial', true).warn).toBe(true)
  })
  describe('minor namespace churn', () => {
    const churn = (dangling: number, unknown = 4096) => ({ ...evidence(unknown, GiB * 100, true), namespace_churn: { dangling_entries: dangling } })
    const partial = (attribution: ReturnType<typeof churn> | ReturnType<typeof evidence>, completion?: string, hasErrors = false) =>
      reportCompleteness({ consistency: 'partial', attribution }, completion, hasErrors)

    it('shows a muted line, not the banner, for 1 to 10 dangling entries', () => {
      for (const n of [1, 2, 10]) expect(partial(churn(n))).toMatchObject({ warn: false, churnEntries: n })
      expect(visibleConsistency({ consistency: 'partial', attribution: churn(1) })).toBe('changed-during-scan')
    })
    it('keeps the banner for 11 dangling entries and for bad counts', () => {
      for (const n of [11, 500, 0, -1, 1.5, Number.NaN]) expect(partial(churn(n))).toMatchObject({ warn: true, churnEntries: 0 })
      expect(visibleConsistency({ consistency: 'partial', attribution: churn(11) })).toBe('partial')
    })
    it('keeps the banner without churn evidence, with a gap the backend did not clear, or on a legacy answer', () => {
      expect(partial(evidence(4096, GiB * 100, true)).warn).toBe(true)
      expect(partial({ ...churn(1), namespace_churn: null } as never).warn).toBe(true)
      expect(reportCompleteness({ consistency: 'partial' }).warn).toBe(true)
    })
    it('never hides a failed or interrupted scan or a per-answer error', () => {
      for (const completion of ['failed', 'cancelled', 'paused', 'runtime_limit', 'resource_limit', 'interrupted']) {
        expect(partial(churn(1), completion)).toMatchObject({ warn: true, churnEntries: 0 })
      }
      expect(partial(churn(1), 'partial', true)).toMatchObject({ warn: true, churnEntries: 0 })
    })
    it('shows the unattributed line as well when unknown bytes are above the small threshold', () => {
      expect(partial(churn(2, 5 * GiB))).toMatchObject({ warn: false, churnEntries: 2, unknownNote: true })
    })
    it('does not treat an unmeasurable partial report as minor churn', () => {
      expect(reportCompleteness({ consistency: 'partial', attribution: { ...churn(1), used_bytes: null } }).warn).toBe(true)
    })
  })
})
