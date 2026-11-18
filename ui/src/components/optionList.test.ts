import {
  filterOptions,
  groupOptions,
  matchesQuery,
  pageCount,
  pageSlice,
  selectableValues,
  toggledValues,
  volumeLayout,
  type PickerOption,
} from './optionList'

const option = (value: string, extra: Partial<PickerOption> = {}): PickerOption => ({ value, label: value, ...extra })

describe('volumeLayout', () => {
  it('keeps tabs up to four volumes', () => {
    expect(volumeLayout(1)).toBe('tabs')
    expect(volumeLayout(4)).toBe('tabs')
  })

  it('uses one dropdown for five to twenty volumes', () => {
    expect(volumeLayout(5)).toBe('dropdown')
    expect(volumeLayout(8)).toBe('dropdown')
    expect(volumeLayout(20)).toBe('dropdown')
  })

  it('uses a searchable select above twenty volumes', () => {
    expect(volumeLayout(21)).toBe('search')
  })
})

describe('pageCount', () => {
  it('shows a short list whole', () => {
    expect(pageCount(200)).toBe(1)
  })

  it('pages fifty at a time above two hundred rows', () => {
    expect(pageCount(201)).toBe(5)
    expect(pageCount(300)).toBe(6)
  })
})

describe('filterOptions', () => {
  const options = [option('share01'), option('Volume1'), option('photos')]

  it('returns every option without a query', () => {
    expect(filterOptions(options, '  ')).toEqual(options)
  })

  it('matches the label case-insensitively', () => {
    expect(filterOptions(options, 'VOL').map(o => o.value)).toEqual(['Volume1'])
    expect(filterOptions(options, 'ot').map(o => o.value)).toEqual(['photos'])
  })

  it('ignores the group and meta text', () => {
    const withMeta = [option('a', { group: 'volume1', meta: '10 GB' })]
    expect(matchesQuery(withMeta[0], 'volume1')).toBe(false)
    expect(matchesQuery(withMeta[0], '10 GB')).toBe(false)
  })
})

describe('groupOptions', () => {
  it('keeps first-seen order and groups ungrouped options alone', () => {
    const grouped = groupOptions([
      option('a', { group: 'volume2' }),
      option('b', { group: 'volume1' }),
      option('c', { group: 'volume2' }),
      option('d'),
    ])
    expect(grouped.map(group => group.label)).toEqual(['volume2', 'volume1', null])
    expect(grouped[0].options.map(o => o.value)).toEqual(['a', 'c'])
  })
})

describe('pageSlice', () => {
  it('returns the requested page', () => {
    const items = Array.from({ length: 120 }, (_, index) => index)
    expect(pageSlice(items, 0).length).toBe(50)
    expect(pageSlice(items, 2)).toEqual(items.slice(100, 120))
  })
})

describe('selectableValues', () => {
  it('leaves out blocked options', () => {
    expect(selectableValues([option('a'), option('b', { disabledReason: 'no space' })])).toEqual(['a'])
  })
})

describe('toggledValues', () => {
  it('replaces the value in a single-choice list', () => {
    expect(toggledValues(['a'], 'b', false)).toEqual(['b'])
  })

  it('adds and removes values in a multi-choice list', () => {
    expect(toggledValues(['a'], 'b', true)).toEqual(['a', 'b'])
    expect(toggledValues(['a', 'b'], 'a', true)).toEqual(['b'])
  })
})
