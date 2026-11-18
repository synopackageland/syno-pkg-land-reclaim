import { mount } from '@vue/test-utils'
import ProgressRing from './ProgressRing.vue'
import ScanField from './ScanField.vue'

describe('ProgressRing', () => {
  it('shows the given progress on the scan field', () => {
    const wrapper = mount(ProgressRing, { props: { ratio: 0.25, label: 'Scan progress', mode: 'fast', density: 1 } })
    expect(wrapper.findAll('[role="progressbar"]')).toHaveLength(1)
    expect(wrapper.get('[role="progressbar"]').attributes('aria-valuenow')).toBe('25')
    expect(wrapper.getComponent(ScanField).props()).toMatchObject({ ratio: 0.25, mode: 'fast', density: 1, paused: false })
  })

  it('stills the field while the scan is paused', () => {
    const wrapper = mount(ProgressRing, { props: { ratio: 0.5, label: 'Scan progress', paused: true } })
    expect(wrapper.get('.scan-field').classes()).toContain('paused')
  })

  it('draws the scan field in the scan mode when no basis exists', () => {
    const wrapper = mount(ProgressRing, { props: { ratio: null, label: 'Scan progress', mode: 'fast', density: 3 } })
    expect(wrapper.get('[role="progressbar"]').attributes('aria-valuenow')).toBeUndefined()
    expect(wrapper.find('.ring-value').exists()).toBe(false)
    expect(wrapper.getComponent(ScanField).props()).toMatchObject({ label: 'Scan progress', mode: 'fast', density: 3 })
  })

})
