import {flushPromises} from '@vue/test-utils'
import type {App} from 'vue'
import {bindChunkRecovery} from './chunkRecovery'

test('a missing lazy module reloads only after the SDK confirms an upgraded UI', async () => {
  const prior = vi.fn()
  const app = {config: {errorHandler: prior}} as unknown as App
  const check = vi.fn(async () => true)
  const reload = vi.fn()
  const stop = bindChunkRecovery(app, {uiUpdate: {check}}, reload)
  const error = new TypeError('Failed to fetch dynamically imported module: https://app.example/chunks/old.js')
  app.config.errorHandler!(error, null, 'async component loader')
  app.config.errorHandler!(error, null, 'async component loader')
  await flushPromises()
  expect(check).toHaveBeenCalledTimes(1)
  expect(reload).toHaveBeenCalledTimes(1)
  expect(prior).not.toHaveBeenCalled()
  stop()
  expect(app.config.errorHandler).toBe(prior)
})

test.each([false, 'unavailable'])('an import failure with UI status %s keeps the existing error path', async status => {
  const prior = vi.fn()
  const app = {config: {errorHandler: prior}} as unknown as App
  const check = status === 'unavailable' ? vi.fn().mockRejectedValue(new Error('offline')) : vi.fn(async () => status)
  const reload = vi.fn()
  bindChunkRecovery(app, {uiUpdate: {check}}, reload)
  const error = new TypeError('Failed to fetch dynamically imported module: https://app.example/chunks/old.js')
  app.config.errorHandler!(error, null, 'async component loader')
  await flushPromises()
  expect(reload).not.toHaveBeenCalled()
  expect(prior).toHaveBeenCalledWith(error, null, 'async component loader')
})

test('ordinary App errors are forwarded without checking or reloading', () => {
  const prior = vi.fn()
  const app = {config: {errorHandler: prior}} as unknown as App
  const check = vi.fn()
  const reload = vi.fn()
  bindChunkRecovery(app, {uiUpdate: {check}}, reload)
  const error = new Error('unrelated failure')
  app.config.errorHandler!(error, null, 'event handler')
  expect(prior).toHaveBeenCalledWith(error, null, 'event handler')
  expect(check).not.toHaveBeenCalled()
  expect(reload).not.toHaveBeenCalled()
})
