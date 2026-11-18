import { afterEach, expect, it, vi } from 'vitest'
import { defineComponent } from 'vue'
import { createUiApp } from './createUiApp'

afterEach(() => vi.unstubAllGlobals())

it('mounts over DSM HTTP with unique prefixes when randomUUID is unavailable', () => {
  const getRandomValues = crypto.getRandomValues.bind(crypto)
  vi.stubGlobal('crypto', { getRandomValues })
  const component = defineComponent({ template: '<div />' })
  const first = createUiApp(component)
  const second = createUiApp(component)
  expect(first.config.idPrefix).toMatch(/^synoreclaim-[a-f0-9]{32}-$/)
  expect(second.config.idPrefix).not.toBe(first.config.idPrefix)
  const root = document.createElement('div')
  first.mount(root)
  expect(root.querySelector('div')).not.toBeNull()
  first.unmount()
})
