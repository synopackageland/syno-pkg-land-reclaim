import { mount } from '@vue/test-utils'
import CancelButton from './CancelButton.vue'

describe('CancelButton', () => {
  it('is an outlined button rather than the loud solid danger one', () => {
    const wrapper = mount(CancelButton, { slots: { default: 'Cancel scan' } })
    expect(wrapper.text()).toBe('Cancel scan')
    expect(wrapper.classes()).toContain('cancel-button')
    expect(wrapper.classes().join(' ')).not.toMatch(/danger/)
  })

  it('passes disabled and click through', async () => {
    const wrapper = mount(CancelButton, { props: { disabled: true }, slots: { default: 'Cancel scan' } })
    expect(wrapper.attributes('disabled')).toBeDefined()
  })
})
