import { HEADER_CHROME, TABLE_EDGE, gridRoom, headerWidth } from './listLayout'

describe('headerWidth', () => {
  const pixels = (text: string) => text.length * 7
  it('keeps the column width a short header fits in', () => {
    expect(headerWidth(96, 'Size', pixels)).toBe(96)
  })

  it('widens the column to its header and the sort arrow', () => {
    expect(headerWidth(96, 'Freigebbar', pixels)).toBe(70 + HEADER_CHROME)
  })

  it('keeps the width without a measured font', () => {
    expect(headerWidth(96, 'Freigebbar')).toBe(96)
  })
})

// At 1440 px the columns filled the grid's host, so its vertical
// scrollbar pushed the table sideways in every language.
describe('gridRoom', () => {
  it('leaves the vertical scrollbar and the table edge out of the columns', () => {
    expect(gridRoom(771, 15)).toBe(771 - 15 - TABLE_EDGE)
  })

  it('leaves only the table edge out with overlay scrollbars', () => {
    expect(gridRoom(771, 0)).toBe(771 - TABLE_EDGE)
  })
})
