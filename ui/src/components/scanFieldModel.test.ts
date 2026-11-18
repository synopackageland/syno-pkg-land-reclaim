import { densityForRate, scanRate } from './scanFieldModel'

describe('densityForRate', () => {
  it('is sparse, middle or dense by the items read per second', () => {
    expect(densityForRate(null)).toBe(2)
    expect(densityForRate(0)).toBe(1)
    expect(densityForRate(1_999)).toBe(1)
    expect(densityForRate(2_000)).toBe(2)
    expect(densityForRate(8_000)).toBe(3)
  })
})

describe('scanRate', () => {
  it('is the items read per second between two samples', () => {
    expect(scanRate({ entries: 1_000, at: 0 }, { entries: 6_000, at: 2_000 })).toBe(2_500)
  })

  it('is unknown without two samples a moment apart, or when the count goes back', () => {
    expect(scanRate(null, { entries: 6_000, at: 2_000 })).toBeNull()
    expect(scanRate({ entries: 1_000, at: 2_000 }, { entries: 6_000, at: 2_100 })).toBeNull()
    expect(scanRate({ entries: 6_000, at: 0 }, { entries: 1_000, at: 2_000 })).toBeNull()
  })
})
