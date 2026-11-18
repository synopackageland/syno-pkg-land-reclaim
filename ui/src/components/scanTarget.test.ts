import { SCAN_TARGET_LIMIT, exceedsTargetLimit, scopeLabel, targetName } from './scanTarget'
import type { Path } from '@/types'

const path = (value: string): Path => ({ path: value })

describe('targetName', () => {
  it('takes the last path segment', () => {
    expect(targetName('/volume1/share01')).toBe('share01')
    expect(targetName('/volume1')).toBe('volume1')
  })
})

describe('scopeLabel', () => {
  it('names a whole volume', () => {
    expect(scopeLabel([path('/volume1')], true)).toBe('Whole volume1')
  })

  it('names one share and counts the rest', () => {
    expect(scopeLabel([path('/volume1/share01')], false)).toBe('share01')
    expect(scopeLabel([path('/volume1/share01'), path('/volume1/share02')], false)).toBe('share01 and 1 more')
  })
})

describe('exceedsTargetLimit', () => {
  it('allows up to the WebAPI limit', () => {
    expect(exceedsTargetLimit(SCAN_TARGET_LIMIT)).toBe(false)
  })

  it('rejects one target more', () => {
    expect(exceedsTargetLimit(SCAN_TARGET_LIMIT + 1)).toBe(true)
  })
})
