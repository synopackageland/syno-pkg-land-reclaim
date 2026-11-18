import { EXPLORE_SPLIT, EXPLORE_SPLIT_GAP, EXPLORE_STACK_BELOW, NAME_FULL_MIN, WHERE_MAX_WIDTH, WHERE_MIN_WIDTH, exploreSplitDefault, exploreStacked, listColumns } from './exploreLayout'
import { gridRoom } from '@/components/listLayout'

describe('exploreStacked', () => {
  it('stacks below the threshold and keeps the side-by-side layout at or above it', () => {
    expect(exploreStacked(EXPLORE_STACK_BELOW - 1)).toBe(true)
    expect(exploreStacked(EXPLORE_STACK_BELOW)).toBe(false)
    expect(exploreStacked(Number.POSITIVE_INFINITY)).toBe(false)
  })
})

// jsdom has no layout, so pin the rule: stacked on a phone, the panel grows
// with its wrapped legend and the map keeps its own height instead of the
// 44 px a fixed-height panel left it at 390 px.
describe('Explore split', () => {
  it('stacks before the side-by-side minimums stop fitting', () => {
    const { minStart, minEnd } = EXPLORE_SPLIT.horizontal
    expect(EXPLORE_STACK_BELOW).toBeGreaterThanOrEqual(minStart + minEnd + EXPLORE_SPLIT_GAP)
  })

  it('gives the treemap the larger share side by side and keeps a usable map height stacked', () => {
    expect(EXPLORE_SPLIT.horizontal.defaultSize).toBeGreaterThanOrEqual(0.55)
    expect(EXPLORE_SPLIT.horizontal.defaultSize).toBeLessThanOrEqual(0.6)
    expect(EXPLORE_SPLIT.vertical.minStart).toBeGreaterThanOrEqual(150)
  })

  // A 1000 px DSM window leaves about 750 px of content: still side by side.
  it('keeps a 1000 px window side by side', () => {
    expect(exploreStacked(740)).toBe(false)
    expect(EXPLORE_SPLIT.horizontal.minEnd).toBeLessThanOrEqual(420)
    expect(EXPLORE_SPLIT.horizontal.minStart).toBeLessThanOrEqual(260)
  })

  it('keeps the stacked map low so the list keeps its rows', () => {
    expect(EXPLORE_SPLIT.vertical.defaultSize).toBeLessThanOrEqual(0.3)
    expect(EXPLORE_SPLIT.vertical.minStart).toBeLessThanOrEqual(190)
  })
})

// At 1440 px "Freigebbar" (de) and "Dimensione" (it) were cut off
// in their fixed columns, beside the sort arrow.
describe('listColumns', () => {
  const held = 128
  it('drops Held by and Where to manage into the name line when the name would be short', () => {
    expect(listColumns(NAME_FULL_MIN + held + WHERE_MIN_WIDTH - 1, held)).toEqual({ name: NAME_FULL_MIN + held + WHERE_MIN_WIDTH - 1, held: null, where: null })
  })

  it('shows every column once the name keeps its width', () => {
    const room = NAME_FULL_MIN + held + WHERE_MIN_WIDTH
    expect(listColumns(room, held)).toEqual({ name: NAME_FULL_MIN, held, where: WHERE_MIN_WIDTH })
  })

  it('gives Where to manage up to its maximum and the name the rest', () => {
    const columns = listColumns(1200, held)
    expect(columns.where).toBe(WHERE_MAX_WIDTH)
    expect(columns.name).toBe(1200 - held - WHERE_MAX_WIDTH)
  })

  it('shows every column before the grid is measured', () => {
    expect(listColumns(Number.POSITIVE_INFINITY, held)).toEqual({ name: 300, held, where: WHERE_MIN_WIDTH })
  })
})

// B14: a maximized 1440 px DSM window leaves Explore about 1170 px. The list
// starts wide enough for Held by and Where to manage as columns, so nothing
// is folded into a cut-off second line; the treemap keeps at least 40%.
describe('Explore split default', () => {
  const listColumnsAt = (width: number) => {
    const share = exploreSplitDefault('horizontal', width)
    const list = (width - EXPLORE_SPLIT_GAP) * (1 - share) - 2
    return listColumns(gridRoom(list, 17) - 40 - 80 - 96, 128)
  }

  it('gives the list its full columns in a maximized 1440 px window', () => {
    expect(listColumnsAt(1170).held).not.toBeNull()
    expect(listColumnsAt(1170).where).not.toBeNull()
  })

  it('keeps the treemap at most 56% and at least 40%, and the stacked default', () => {
    expect(exploreSplitDefault('horizontal', 2400)).toBe(EXPLORE_SPLIT.horizontal.defaultSize)
    expect(exploreSplitDefault('horizontal', 900)).toBe(0.4)
    expect(exploreSplitDefault('horizontal', Infinity)).toBe(EXPLORE_SPLIT.horizontal.defaultSize)
    expect(exploreSplitDefault('vertical', 600)).toBe(EXPLORE_SPLIT.vertical.defaultSize)
  })
})
