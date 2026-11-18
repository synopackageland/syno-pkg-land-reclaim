import { mount } from '@vue/test-utils'
import { Badge } from '@synopackageland/ui'
import ScanBadge from './ScanBadge.vue'
import PageStatus from './PageStatus.vue'
import { openPageStatus } from './pageInfoTesting'

const NOW = new Date('2026-10-15T12:00:00Z')
const report = (extra = {}) => ({ scan_finished: '2026-10-15T10:00:00Z', consistency: 'observed-stable', ...extra })

describe('ScanBadge', () => {
  beforeEach(() => { vi.useFakeTimers({ now: NOW }) })
  afterEach(() => {
    vi.useRealTimers()
    document.body.innerHTML = ''
  })

  it('names the page status by when the scan finished and how it read, in the tone of that reading', () => {
    const status = mount(ScanBadge, { props: { report: report() } }).getComponent(PageStatus)
    expect(status.props('label')).toMatch(/ · No changes seen$/)
    expect(status.props('tone')).toBe('success')
  })

  it('warns about an old report whatever its reading', () => {
    const status = mount(ScanBadge, { props: { report: report({ stale: true }) } }).getComponent(PageStatus)
    expect(status.props('tone')).toBe('warning')
  })

  it('shows data that changed during the scan as information', () => {
    const status = mount(ScanBadge, { props: { report: report({ consistency: 'changed-during-scan' }) } }).getComponent(PageStatus)
    expect(status.props('tone')).toBe('info')
  })

  it('takes the reading a page settled on', () => {
    const status = mount(ScanBadge, { props: { report: report(), consistency: 'partial' } }).getComponent(PageStatus)
    expect(status.props('label')).toContain('Not fully read')
    expect(status.props('tone')).toBe('warning')
  })

  // Status is not an action: it sits in the page bar's meta (SDK PageInfo), so on a
  // phone it never floats over the list as the actions do.
  it('opens the badge, its details and the page notes from one info button in the page meta', async () => {
    const wrapper = mount(ScanBadge, { props: { report: report({ stale: true }), details: 'Why', notes: ['A note', ''] }, attachTo: document.body })
    const button = wrapper.get('[data-testid="page-status"]')
    expect(button.classes()).toContain('page-info')
    expect(button.attributes('aria-label')).toMatch(/ · No changes seen$/)
    expect(button.attributes('data-tone')).toBe('warning')
    expect(wrapper.findComponent(Badge).exists()).toBe(false)
    const opened = await openPageStatus()
    expect(opened.text).toContain('No changes seen')
    expect(opened.text).toContain('Why')
    expect(opened.notes).toEqual(['A note'])
    wrapper.unmount()
  })
})
