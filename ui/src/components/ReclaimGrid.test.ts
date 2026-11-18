import { mount } from '@vue/test-utils'
import { DataGrid } from '@synopackageland/ui'
import ReclaimGrid from './ReclaimGrid.vue'
import { gridLabels } from './gridLabels'
import { catalogs } from '@/i18n/catalogs'
import { createTranslator, type Locale } from '@/i18n/t'

const columns = [{ key: 'name', label: 'Name', sortable: true, width: 120, minWidth: 80 }]
const rows = [{ id: 'b', name: 'Beta' }, { id: 'a', name: 'Alpha' }]
const props = { tableId: 'test', columns, rows }

beforeEach(() => localStorage.clear())

it('keeps the given order, supports keyboard sort and shows a localised count', async () => {
  const wrapper = mount(ReclaimGrid, { props })
  expect(wrapper.findAll('tbody tr')[0].text()).toBe('Beta')
  await wrapper.find('th[data-column="name"]').trigger('keydown', { key: 'Enter' })
  expect(wrapper.findAll('tbody tr')[0].text()).toBe('Alpha')
  expect(wrapper.find('th').attributes('aria-sort')).toBe('ascending')
  expect(wrapper.text()).toContain('2 items')
  wrapper.unmount()
})

it('persists keyboard widths for only this table and restores them after remount', async () => {
  let wrapper = mount(ReclaimGrid, { props })
  await wrapper.find('[data-resize-handle]').trigger('keydown', { key: 'ArrowRight' })
  wrapper.unmount()
  wrapper = mount(ReclaimGrid, { props })
  expect(wrapper.findComponent(DataGrid).props('columnWidths')).toEqual({ name: 130 })
  const other = mount(ReclaimGrid, { props: { ...props, tableId: 'other' } })
  expect(other.findComponent(DataGrid).props('columnWidths')).toEqual({})
  wrapper.unmount(); other.unmount()
})

it('ignores malformed storage and keeps resizing usable when storage is denied', async () => {
  localStorage.setItem('synoreclaim.grid.test.widths', '{broken')
  const wrapper = mount(ReclaimGrid, { props })
  vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => { throw new Error('blocked') })
  await wrapper.find('[data-resize-handle]').trigger('keydown', { key: 'ArrowRight' })
  expect(wrapper.findComponent(DataGrid).props('columnWidths')).toEqual({ name: 130 })
  wrapper.unmount(); vi.restoreAllMocks()
})

it('moves pages and returns to a valid page after the rows shrink', async () => {
  const wrapper = mount(ReclaimGrid, { props: { ...props, pageSize: 1 } })
  await wrapper.find('button[aria-label="Next page"]').trigger('click')
  expect(wrapper.find('tbody').text()).toBe('Alpha')
  await wrapper.setProps({ rows: [rows[0]] })
  expect(wrapper.find('tbody').text()).toBe('Beta')
  wrapper.unmount()
})

it.each(Object.keys(catalogs) as Locale[])('localises every grid control for %s', locale => {
  const translator = createTranslator(locale, catalogs)
  const labels = gridLabels(translator.t, new Intl.NumberFormat(locale).format)
  expect(labels.items(1200)).toContain(new Intl.NumberFormat(locale).format(1200))
  expect(labels.page(2, 3)).toContain('2')
  expect(labels.selectRow('file')).toContain('file')
  expect(labels.resizeColumn('Size')).toContain('Size')
  for (const value of Object.values(labels)) {
    const text = typeof value === 'function' ? (value as (n: number, total: number) => string)(2, 3) : value
    expect(text).not.toMatch(/common\.|\{\w+\}/)
  }
})
