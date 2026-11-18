import { mount } from '@vue/test-utils'
import { blockingState } from './states'
import StateView from './StateView.vue'

describe('blockingState (in order)', () => {
  const volumesDoc = (count: number) => ({ volumes: new Array(count).fill({}) })

  it('puts not_admin and backend_unavailable first', () => {
    expect(blockingState('not_admin', null)).toBe('not_admin')
    expect(blockingState('backend_unavailable', volumesDoc(1))).toBe('backend_unavailable')
  })

  it('reports no_supported_volume when volumes.list has no scannable volume', () => {
    expect(blockingState('ready', volumesDoc(0))).toBe('no_supported_volume')
  })

  it('allows ext4 scans and rejects volumes without scan capability', () => {
    expect(blockingState('ready', { volumes: [{ filesystem: 'ext4', capabilities: { scan: true } }] })).toBeNull()
    expect(blockingState('ready', { volumes: [{ filesystem: 'ext4', capabilities: { scan: false } }] })).toBe('no_supported_volume')
  })

  it('is null when the app can show pages', () => {
    expect(blockingState('ready', volumesDoc(1))).toBeNull()
    expect(blockingState('ready', null)).toBeNull()
  })
})

describe('StateView', () => {
  it('renders a blocking state as an empty state', () => {
    const wrapper = mount(StateView, { props: { kind: 'not_admin' } })
    expect(wrapper.text()).toContain('Only DSM administrators can use Syno Reclaim')
  })

  it('renders partial as a warning callout with localized reason codes', () => {
    const wrapper = mount(StateView, { props: { kind: 'partial', codes: [{ set: 'completion', value: 'partial' }, { set: 'completion', value: 'weird' }] } })
    expect(wrapper.text()).toContain('Some data could not be read')
    expect(wrapper.text()).toContain('Partial')
    expect(wrapper.text()).toContain('Unrecognised status (weird)')
  })

  it('fills the last scan duration of no_report', () => {
    const wrapper = mount(StateView, { props: { kind: 'no_report', lastScanSeconds: 790 } })
    expect(wrapper.text()).toContain('The last full scan took 13:10')
  })
})
