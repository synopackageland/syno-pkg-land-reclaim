import { donutArcs, linePaths, shareLabel } from './charts'

describe('donutArcs', () => {
  it('splits the circumference by value, each arc starting where the last ended', () => {
    expect(donutArcs([1, 3], 100)).toEqual([
      { length: 25, offset: 0 },
      { length: 75, offset: -25 },
    ])
  })

  it('draws nothing for an empty total', () => {
    expect(donutArcs([0, 0], 100)).toEqual([
      { length: 0, offset: 0 },
      { length: 0, offset: 0 },
    ])
  })

  it('gives a tiny value the minimum length and takes it from the larger arcs', () => {
    const arcs = donutArcs([1, 0, 999], 100, { minLength: 4 })
    expect(arcs.map(a => a.length)).toEqual([4, 0, 96])
    expect(arcs.map(a => a.offset)).toEqual([0, -4, -4])
  })

  it('keeps raising arcs that the shrinking pushed under the minimum', () => {
    const lengths = donutArcs([1, 1, 5, 93], 100, { minLength: 6 }).map(a => Math.round(a.length * 100) / 100)
    expect(lengths).toEqual([6, 6, 6, 82])
  })

  it('shares the circle equally when every arc would be under the minimum', () => {
    expect(donutArcs([1, 2], 10, { minLength: 6 }).map(a => a.length)).toEqual([5, 5])
  })
})

describe('shareLabel', () => {
  const percent = (ratio: number) => `${(ratio * 100).toFixed(1)}%`

  it('formats a part of a whole', () => {
    expect(shareLabel(25, 100, percent)).toBe('25.0%')
  })

  it('says a visible but tiny part is under the smallest step instead of 0%', () => {
    expect(shareLabel(1, 1_000_000, percent)).toBe('<0.1%')
    expect(shareLabel(0, 100, percent)).toBe('0.0%')
  })

  it('has no share without a whole', () => {
    expect(shareLabel(1, 0, percent)).toBeUndefined()
  })
})

describe('linePaths', () => {
  const box = { width: 100, height: 50, xMax: 10 }

  it('scales x to the domain and puts the largest y at the top', () => {
    const { line, area } = linePaths([{ x: 0, y: 0 }, { x: 5, y: 10 }, { x: 10, y: 5 }], box)
    expect(line).toBe('M0 50 L50 0 L100 25')
    expect(area).toBe('M0 50 L50 0 L100 25 L100 50 L0 50 Z')
  })

  it('gives the position of the last point', () => {
    expect(linePaths([{ x: 0, y: 0 }, { x: 5, y: 10 }], box).end).toEqual({ x: 50, y: 0 })
  })

  it('draws a flat series across the middle', () => {
    expect(linePaths([{ x: 0, y: 7 }, { x: 10, y: 7 }], box).line).toBe('M0 25 L100 25')
  })

  it('centres a change smaller than minYSpan in a range of that span', () => {
    expect(linePaths([{ x: 0, y: 99 }, { x: 10, y: 101 }], { ...box, minYSpan: 20 }).line).toBe('M0 27.5 L100 22.5')
  })

  it('fits the data when it spans more than minYSpan', () => {
    expect(linePaths([{ x: 0, y: 0 }, { x: 10, y: 10 }], { ...box, minYSpan: 5 }).line).toBe('M0 50 L100 0')
  })

  it('is empty without points', () => {
    expect(linePaths([], box)).toEqual({ line: '', area: '', end: null })
  })
})
