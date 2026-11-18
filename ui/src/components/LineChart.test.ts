import { mount } from '@vue/test-utils'
import LineChart from './LineChart.vue'

const props = { points: [{ x: 0, y: 1 }, { x: 10, y: 3 }], xMax: 10, title: 'Used space', width: 100, height: 50 }

describe('LineChart', () => {
  it('titles the chart and labels the x axis', () => {
    const wrapper = mount(LineChart, { props: { ...props, xLabels: ['09/28', '09/29'] } })
    expect(wrapper.find('svg title').text()).toBe('Used space')
    expect(wrapper.findAll('figcaption span').map(span => span.text())).toEqual(['09/28', '09/29'])
  })

  it('marks the last point with its value when given one', () => {
    const wrapper = mount(LineChart, { props: { ...props, lastLabel: '3 B' } })
    expect(wrapper.find('circle').attributes()).toMatchObject({ cx: '100', cy: '0' })
    expect(wrapper.find('text').text()).toBe('3 B')
  })

  it('draws no marker without a label', () => {
    expect(mount(LineChart, { props }).find('circle').exists()).toBe(false)
  })
})
