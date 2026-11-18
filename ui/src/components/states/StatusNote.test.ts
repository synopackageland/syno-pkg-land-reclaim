import { mount } from '@vue/test-utils'
import { Tooltip } from '@synopackageland/ui'
import StatusNote from './StatusNote.vue'

describe('StatusNote', () => {
  it('renders a muted labelled line with a focusable details trigger', () => {
    const wrapper = mount(StatusNote, {
      props: { label: 'Some data could not be read', details: ['Partial', 'The age estimate is unavailable'], testid: 'coverage' },
    })
    const note = wrapper.find('[data-testid="coverage"]')
    expect(note.exists()).toBe(true)
    expect(note.classes()).toContain('status-note')
    expect(note.text()).toContain('Some data could not be read')
    expect(note.find('[tabindex="0"]').exists()).toBe(true)
    expect(String(wrapper.findComponent(Tooltip).props('content'))).toContain('The age estimate is unavailable')
  })

  it('renders a plain line without a details trigger when there is nothing to explain', () => {
    const wrapper = mount(StatusNote, { props: { label: 'Quiet status' } })
    expect(wrapper.text()).toContain('Quiet status')
    expect(wrapper.find('[tabindex="0"]').exists()).toBe(false)
    expect(wrapper.findComponent(Tooltip).exists()).toBe(false)
  })
})
