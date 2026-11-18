import { mount } from '@vue/test-utils'
import PartialHint from './PartialHint.vue'

describe('PartialHint', () => {
  it('says in one quiet line that some data could not be read, and why', () => {
    const hint = mount(PartialHint, { props: { details: ['Unknown parts are never counted as reclaimable'] } }).get('[data-testid="partial-hint"]')
    expect(hint.element.tagName).toBe('P')
    expect(hint.text()).toBe('Some data could not be read · Unknown parts are never counted as reclaimable')
  })
})
