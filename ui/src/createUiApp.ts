import { createApp, type Component } from 'vue'

/** Vue's generated accessibility IDs must be unique across DSM windows. */
export function createUiApp(component: Component) {
  const app = createApp(component)
  // DSM also serves HTTP, where randomUUID is unavailable. getRandomValues
  // provides the same 128-bit random namespace without a secure-context rule.
  const nonce = crypto.getRandomValues(new Uint8Array(16))
  app.config.idPrefix = `synoreclaim-${Array.from(nonce, byte => byte.toString(16).padStart(2, '0')).join('')}-`
  return app
}
