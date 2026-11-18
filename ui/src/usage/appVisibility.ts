// DSM minimizes an Ext window without hiding the browser document.
import type { InjectionKey, Ref } from 'vue'

export const APP_VISIBLE: InjectionKey<Ref<boolean>> = Symbol('appVisible')
export interface AppWindowVisibility {
  minimized?: boolean
  isVisible?: () => boolean
  on: (event: string, callback: () => void) => void
  un: (event: string, callback: () => void) => void
}

export function bindAppVisibility(appWindow: AppWindowVisibility, visible: Ref<boolean>) {
  visible.value = !appWindow.minimized && appWindow.isVisible?.() !== false
  const show = () => { visible.value = true }
  const hide = () => { visible.value = false }
  const events: [string, () => void][] = [['show', show], ['restore', show], ['hide', hide], ['minimize', hide]]
  for (const [event, callback] of events) appWindow.on(event, callback)
  return () => { for (const [event, callback] of events) appWindow.un(event, callback) }
}

/**
 * DSM can reuse an AppWindow (and the Vue app inside it) after a hide or
 * minimize; showing it again is a new opening, so it must check authorization
 * again.
 */
export function bindAppReopen(appWindow: AppWindowVisibility, reopen: () => void) {
  const events = ['show', 'restore']
  for (const event of events) appWindow.on(event, reopen)
  return () => { for (const event of events) appWindow.un(event, reopen) }
}
