import { readFileSync } from 'node:fs'
import { mount } from '@vue/test-utils'
import { nextTick } from 'vue'
import ScanField from './ScanField.vue'

const style = () => readFileSync('src/components/ScanField.vue', 'utf8')

describe('ScanField', () => {
  it('is an indeterminate progress bar with its label', () => {
    const wrapper = mount(ScanField, { props: { label: 'Scan progress', mode: 'gentle', density: 2 } })
    const bar = wrapper.get('[role="progressbar"]')
    expect(bar.attributes('aria-label')).toBe('Scan progress')
    expect(bar.attributes('aria-valuenow')).toBeUndefined()
  })

  it('draws a few large soft glows, not a field of small shapes', () => {
    const wrapper = mount(ScanField, { props: { label: 'Scan', mode: 'gentle', density: 2 } })
    expect(wrapper.findAll('.field-layer.gentle .field-glow')).toHaveLength(3)
    expect(wrapper.find('path').exists()).toBe(false)
  })

  it('draws the known progress as an arc with no track', () => {
    const wrapper = mount(ScanField, { props: { label: 'Scan progress', mode: 'gentle', density: 2, ratio: 0.25 } })
    const bar = wrapper.get('[role="progressbar"]')
    expect(bar.attributes('aria-valuenow')).toBe('25')
    const arc = wrapper.get('.field-value')
    const length = Number(arc.attributes('stroke-dasharray'))
    expect(Number(arc.attributes('stroke-dashoffset'))).toBeCloseTo(length * 0.75)
    expect(wrapper.findAll('circle')).toHaveLength(1)
    expect(style()).not.toMatch(/--ui-border/)
    expect(style()).toMatch(/prefers-reduced-motion[^]*\.field-value\s*\{\s*transition:\s*none/)
  })

  it('has no arc without an estimate', () => {
    const wrapper = mount(ScanField, { props: { label: 'Scan', mode: 'gentle', density: 2 } })
    expect(wrapper.find('.field-value').exists()).toBe(false)
  })

  it('stands still while the scan is paused', () => {
    const wrapper = mount(ScanField, { props: { label: 'Scan', mode: 'gentle', density: 2, paused: true } })
    expect(wrapper.get('.scan-field').classes()).toContain('paused')
  })

  it('shows the layer of the scan mode and cross-fades to the other', async () => {
    vi.useFakeTimers()
    const wrapper = mount(ScanField, { props: { label: 'Scan', mode: 'gentle', density: 2 } })
    expect(wrapper.get('.field-layer.gentle').classes()).toContain('on')
    expect(wrapper.find('.field-layer.fast').exists()).toBe(false)
    await wrapper.setProps({ mode: 'fast' })
    expect(wrapper.get('.field-layer.fast').classes()).toContain('on')
    expect(wrapper.get('.field-layer.gentle').classes()).not.toContain('on')
    expect(style()).toMatch(/\.field-layer\s*\{[^}]*transition:\s*opacity/)
    vi.advanceTimersByTime(700)
    await nextTick()
    expect(wrapper.find('.field-layer.gentle').exists()).toBe(false)
    vi.useRealTimers()
  })

  it('turns faster in fast mode', () => {
    const turn = (mode: string) => Number(style().match(new RegExp(`\\.field-layer\\.${mode}\\s*\\{[^}]*--orbit:\\s*([\\d.]+)s`))?.[1])
    expect(turn('gentle')).toBeGreaterThan(turn('fast') * 3)
  })

  it('glows brighter as the scan reads faster', async () => {
    const wrapper = mount(ScanField, { props: { label: 'Scan', mode: 'fast', density: 1 } })
    expect(wrapper.get('.scan-field').attributes('data-density')).toBe('1')
    await wrapper.setProps({ density: 3 })
    expect(wrapper.get('.scan-field').attributes('data-density')).toBe('3')
    expect(style()).toMatch(/\[data-density="3"\][^{]*\{[^}]*opacity/)
  })

  it('pauses while the page is hidden', async () => {
    const hidden = vi.spyOn(document, 'hidden', 'get').mockReturnValue(true)
    const wrapper = mount(ScanField, { props: { label: 'Scan', mode: 'gentle', density: 2 } })
    expect(wrapper.get('.scan-field').classes()).toContain('paused')
    hidden.mockReturnValue(false)
    document.dispatchEvent(new Event('visibilitychange'))
    await nextTick()
    expect(wrapper.get('.scan-field').classes()).not.toContain('paused')
    hidden.mockRestore()
    expect(style()).toMatch(/\.paused[^{]*\{[^}]*animation-play-state:\s*paused/)
  })

  it('animates only transform and opacity, without filters, and stands still for reduced motion', () => {
    const keyframes = [...style().matchAll(/@keyframes[^{]+\{([^]*?)\n\}/g)].map(match => match[1])
    expect(keyframes.length).toBeGreaterThan(0)
    for (const frames of keyframes) {
      const properties = [...frames.matchAll(/([a-z-]+)\s*:/g)].map(match => match[1])
      expect(properties.every(property => property === 'opacity' || property === 'transform')).toBe(true)
    }
    expect(style()).not.toMatch(/filter:/)
    expect(style()).toMatch(/prefers-reduced-motion: reduce[^]*animation:\s*none/)
  })
})
