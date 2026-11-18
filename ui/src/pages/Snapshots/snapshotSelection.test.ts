import { daysBefore, olderThan, rangeSelection } from './snapshotSelection'

const order = ['a', 'b', 'c', 'd', 'e']
const all = () => true

describe('rangeSelection', () => {
  it('takes a plain toggle as is and anchors it', () => {
    expect(rangeSelection([], ['b'], order, null, all)).toEqual({ keys: ['b'], anchor: 'b' })
  })
  it('fills the range from the anchor on shift', () => {
    expect(rangeSelection(['b'], ['b', 'e'], order, 'b', all)).toEqual({ keys: ['b', 'c', 'd', 'e'], anchor: 'e' })
  })
  it('clears a range when the shift-clicked box was cleared', () => {
    expect(rangeSelection(['a', 'b', 'c', 'd'], ['a', 'b', 'c'], order, 'b', all).keys).toEqual(['a'])
  })
  it('skips rows that cannot be selected', () => {
    expect(rangeSelection(['a'], ['a', 'd'], order, 'a', key => key !== 'c').keys).toEqual(['a', 'b', 'd'])
  })
  it('leaves a select-all alone', () => {
    expect(rangeSelection([], order, order, 'b', all)).toEqual({ keys: order, anchor: null })
  })
})

describe('older than', () => {
  const snapshots = [
    { subvolume_id: 3, otime: '2026-10-03T12:00:00', pending_deletion: false },
    { subvolume_id: 2, otime: '2026-10-02T12:00:00', pending_deletion: true },
    { subvolume_id: 1, otime: '2026-10-01T12:00:00', pending_deletion: false },
  ]
  it('offers the days that have an older snapshot, newest first', () => {
    expect(daysBefore(snapshots).map(day => new Date(day).getDate())).toEqual([3, 2])
  })
  it('selects every snapshot before the day except those being deleted', () => {
    expect(olderThan(snapshots, new Date(2026, 9, 3).getTime())).toEqual(['1'])
  })
})
