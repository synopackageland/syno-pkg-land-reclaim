import type {App} from 'vue'
import type {Context} from '@synopackageland/sdk'

/** An upgraded package removes old chunks; ordinary network failures keep their error path. */
export function bindChunkRecovery(
  app: App,
  ctx: Partial<Pick<Context, 'uiUpdate'>> | undefined,
  reload = () => window.location.reload(),
) {
  const previous = app.config.errorHandler
  let active = true
  let checking = false
  const handler: NonNullable<App['config']['errorHandler']> = (error, instance, info) => {
    const forward = () => {
      if (previous) previous(error, instance, info)
      else console.error(error)
    }
    if (!ctx?.uiUpdate || !(error instanceof Error)
      || !/Failed to fetch dynamically imported module|Importing a module script failed|error loading dynamically imported module/i.test(error.message)) {
      forward()
      return
    }
    if (checking) return
    checking = true
    void ctx.uiUpdate.check().then(stale => {
      if (!active) return
      if (stale) reload()
      else {checking = false; forward()}
    }, () => {
      checking = false
      if (active) forward()
    })
  }
  app.config.errorHandler = handler
  return () => {
    active = false
    if (app.config.errorHandler === handler) app.config.errorHandler = previous
  }
}
