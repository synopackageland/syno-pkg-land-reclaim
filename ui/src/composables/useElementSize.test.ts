import { mount } from '@vue/test-utils'
import { defineComponent, h, ref } from 'vue'
import { useElementSize } from './useElementSize'

type Callback = (entries: { contentRect: { width: number; height: number } }[]) => void
let observers: { callback: Callback; disconnected: boolean }[] = []

class FakeResizeObserver {
  record: { callback: Callback; disconnected: boolean }
  constructor(callback: Callback) {
    this.record = { callback, disconnected: false }
    observers.push(this.record)
  }
  observe() {}
  disconnect() { this.record.disconnected = true }
}

function mountProbe(initial = { width: 470, height: 440 }) {
  let size!: ReturnType<typeof useElementSize>
  const wrapper = mount(defineComponent({
    setup() {
      const host = ref<HTMLElement | null>(null)
      size = useElementSize(host, initial)
      return () => h('div', { ref: host })
    },
  }))
  return { wrapper, size: () => size.value }
}

const resize = (width: number, height: number) => observers[0].callback([{ contentRect: { width, height } }])

describe('useElementSize', () => {
  beforeEach(() => {
    observers = []
    vi.stubGlobal('ResizeObserver', FakeResizeObserver)
  })
  afterEach(() => vi.unstubAllGlobals())

  it('starts from the initial size and follows the element, rounded', () => {
    const { size } = mountProbe()
    expect(size()).toEqual({ width: 470, height: 440 })
    resize(512.4, 300.6)
    expect(size()).toEqual({ width: 512, height: 301 })
  })

  it('ignores a zero size, which a hidden element reports', () => {
    const { size } = mountProbe()
    resize(0, 300)
    expect(size()).toEqual({ width: 470, height: 440 })
  })

  it('stops observing when the component unmounts', () => {
    const { wrapper } = mountProbe()
    wrapper.unmount()
    expect(observers[0].disconnected).toBe(true)
  })

  it('keeps the initial size without ResizeObserver', () => {
    vi.stubGlobal('ResizeObserver', undefined)
    const { size } = mountProbe({ width: 10, height: 20 })
    expect(size()).toEqual({ width: 10, height: 20 })
  })
})
