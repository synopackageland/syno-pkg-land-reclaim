import { expect, it } from 'vitest'
import { ref } from 'vue'
import { bindAppReopen, bindAppVisibility } from './appVisibility'

it('tracks DSM hide/minimize and show/restore, and removes the listeners on close', () => {
  const handlers = new Map<string, () => void>()
  const visible = ref(true)
  const stop = bindAppVisibility({
    minimized: true,
    on: (event, callback) => { handlers.set(event, callback) },
    un: (event) => { handlers.delete(event) },
  }, visible)
  expect(visible.value).toBe(false)
  handlers.get('restore')!()
  expect(visible.value).toBe(true)
  handlers.get('hide')!()
  expect(visible.value).toBe(false)
  handlers.get('show')!()
  expect(visible.value).toBe(true)
  handlers.get('minimize')!()
  expect(visible.value).toBe(false)
  stop()
  expect(handlers.size).toBe(0)
})

it('runs a fresh check when DSM shows or restores a reused window, and stops on close', () => {
  const handlers = new Map<string, () => void>()
  let reopens = 0
  const stop = bindAppReopen({
    on: (event, callback) => { handlers.set(event, callback) },
    un: (event) => { handlers.delete(event) },
  }, () => { reopens += 1 })
  handlers.get('show')!()
  handlers.get('restore')!()
  expect(reopens).toBe(2)
  stop()
  expect(handlers.size).toBe(0)
})
