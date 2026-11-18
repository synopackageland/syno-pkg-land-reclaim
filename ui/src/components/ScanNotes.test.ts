import { mount } from '@vue/test-utils'
import ScanNotes from './ScanNotes.vue'

describe('ScanNotes', () => {
  it('shows nothing without notes', () => {
    expect(mount(ScanNotes, { props: { notes: [] } }).find('details').exists()).toBe(false)
  })

  it('folds the notes behind a summary that counts them, closed by default', () => {
    const wrapper = mount(ScanNotes, { props: { notes: ['First', '', 'Second'] } })
    const details = wrapper.get('details')
    expect(details.attributes('open')).toBeUndefined()
    expect(wrapper.get('summary').text()).toBe('Notes (2)')
    expect(wrapper.findAll('li').map(item => item.text())).toEqual(['First', 'Second'])
  })
})
