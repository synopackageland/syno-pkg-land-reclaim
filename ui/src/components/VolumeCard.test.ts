import { mount } from '@vue/test-utils'
import VolumeCard from './VolumeCard.vue'

const props = { name: 'volume1', chip: 'Stable', usedRatio: 0.5, usedText: '5 GB of 10 GB used', figureLabel: 'Free', figure: '5 GB' }
const bar = (wrapper: ReturnType<typeof mount>) => wrapper.get('[role="progressbar"]')

describe('VolumeCard', () => {
  it('keeps the primary usage bar by default', () => {
    const wrapper = mount(VolumeCard, { props })
    expect(bar(wrapper).attributes('data-tone')).toBe('primary')
    expect(bar(wrapper).attributes('aria-valuenow')).toBe('50')
    expect(bar(wrapper).attributes('aria-label')).toBe('5 GB of 10 GB used')
  })

  it('tints the usage bar with the card tone when asked', () => {
    const wrapper = mount(VolumeCard, { props: { ...props, barTone: 'danger' } })
    expect(bar(wrapper).attributes('data-tone')).toBe('danger')
  })
})
