import { mount } from '@vue/test-utils'
import Donut from './Donut.vue'

const segments = [
  { key: 'a', value: 1, color: 'red', label: 'A', amount: '1 GB', share: '25%', details: ['25% of used'] },
  { key: 'b', value: 3, color: 'blue', label: 'B', amount: '3 GB', share: '75%', details: ['75% of used', '30% of volume'] },
]

const mountDonut = () => mount(Donut, { props: { title: 'Where the space went', segments }, slots: { default: '4 GB' }, attachTo: document.body })

it('draws one named arc per segment, the legend and the centre slot', () => {
  const wrapper = mountDonut()
  const arcs = wrapper.findAll('circle')
  expect(arcs.map(c => c.attributes('aria-label'))).toEqual(['A, 1 GB, 25% of used', 'B, 3 GB, 75% of used, 30% of volume'])
  expect(wrapper.find('svg').attributes('aria-label')).toBe('Where the space went')
  expect(wrapper.findAll('.legend li').map(li => li.text())).toEqual(['A1 GB25%', 'B3 GB75%'])
  expect(wrapper.find('.donut-centre').text()).toBe('4 GB')
  wrapper.unmount()
})

it('shows the name, amount and shares of a hovered arc in the centre and highlights its legend row', async () => {
  const wrapper = mountDonut()
  await wrapper.findAll('circle')[1].trigger('mouseenter')
  expect(wrapper.find('.donut-centre').text()).toBe('B3 GB75% of used30% of volume')
  expect(wrapper.findAll('.legend li')[1].attributes('data-active')).toBe('')
  expect(wrapper.findAll('circle')[0].classes()).toContain('dimmed')
  await wrapper.find('svg').trigger('mouseleave')
  expect(wrapper.find('.donut-centre').text()).toBe('4 GB')
  wrapper.unmount()
})

it('highlights the arc of a hovered legend row', async () => {
  const wrapper = mountDonut()
  await wrapper.findAll('.legend li')[0].trigger('mouseenter')
  expect(wrapper.findAll('circle')[0].classes()).toContain('active')
  expect(wrapper.find('.donut-centre').text()).toContain('A')
  await wrapper.findAll('.legend li')[0].trigger('mouseleave')
  expect(wrapper.findAll('circle')[0].classes()).not.toContain('active')
  wrapper.unmount()
})

it('moves between arcs with the arrow keys, one tab stop for the chart', async () => {
  const wrapper = mountDonut()
  const arcs = () => wrapper.findAll('circle')
  expect(arcs().map(c => c.attributes('tabindex'))).toEqual(['0', '-1'])
  await arcs()[0].trigger('focus')
  expect(wrapper.find('.donut-centre').text()).toContain('A')
  await arcs()[0].trigger('keydown', { key: 'ArrowRight' })
  expect(document.activeElement).toBe(arcs()[1].element)
  expect(arcs().map(c => c.attributes('tabindex'))).toEqual(['-1', '0'])
  await arcs()[1].trigger('keydown', { key: 'ArrowRight' })
  expect(document.activeElement).toBe(arcs()[0].element)
  await arcs()[0].trigger('keydown', { key: 'End' })
  expect(document.activeElement).toBe(arcs()[1].element)
  await arcs()[1].trigger('keydown', { key: 'Escape' })
  expect(wrapper.find('.donut-centre').text()).toBe('4 GB')
  wrapper.unmount()
})

it('keeps a tiny segment visible', () => {
  const wrapper = mount(Donut, {
    props: {
      title: 't',
      segments: [
        { key: 'big', value: 1e12, color: 'red', label: 'Big', amount: '1 TB' },
        { key: 'tiny', value: 1, color: 'blue', label: 'Tiny', amount: '1 B' },
      ],
    },
  })
  const dash = wrapper.findAll('circle')[1].attributes('stroke-dasharray')!
  expect(Number(dash.split(' ')[0])).toBeGreaterThan(3)
})

it('lets the page replace a legend label', () => {
  const wrapper = mount(Donut, { props: { title: 't', segments }, slots: { label: '<em>custom {{ params.segment.key }}</em>' } })
  expect(wrapper.find('.legend em').text()).toBe('custom a')
})

it('clears the highlight when the parts change', async () => {
  const wrapper = mountDonut()
  await wrapper.findAll('circle')[1].trigger('mouseenter')
  await wrapper.setProps({ segments: [{ ...segments[0], key: 'c' }] })
  expect(wrapper.findAll('circle').map(c => c.classes())).toEqual([[]])
  expect(wrapper.find('.donut-centre').text()).toBe('4 GB')
  wrapper.unmount()
})
