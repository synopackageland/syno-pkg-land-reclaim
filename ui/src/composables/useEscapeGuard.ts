/**
 * The DSM AppWindow (Ext.window.Window) closes itself on Escape. With one of the
 * app's dialogs open, that would close the whole app instead of the dialog
 * (confirmed on real DSM).
 *
 * The guard is scoped to this app's own mounted root, so Escape in another DSM
 * app or window is untouched. App-scoped SDK layers own their Escape handling.
 * The legacy window re-dispatch applies only to document-scoped layers.
 * While one of this app's dialogs is open and focus
 * or the event belongs to this app, the capture listener stops the original
 * Escape before it can reach the AppWindow element and re-dispatches it on
 * `window` so the dialog's own dismissal (reka-ui listens on window) still
 * closes the dialog. The re-dispatched event never enters the document, so it
 * cannot recurse.
 */
export const ESCAPE_GUARD_SELECTOR = '[role="dialog"]'

export function installEscapeGuard(root: Element, target: Document = document): () => void {
  const ownsEscape = (event: KeyboardEvent) =>
    root.isConnected
    && root.querySelector(ESCAPE_GUARD_SELECTOR) !== null
    && (root.contains(event.target as Node) || root.contains(target.activeElement))

  const listener = (event: KeyboardEvent) => {
    if (event.key !== 'Escape' || !ownsEscape(event)) return
    // App-scoped SDK layers dismiss at AppRoot capture and stop propagation
    // before DSM's bubble listener. Re-dispatching on window bypasses them.
    if (event.target instanceof Element && event.target.closest('[data-ui-modal-scope="app"]')) return
    event.preventDefault()
    event.stopPropagation()
    target.defaultView?.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }))
  }
  target.addEventListener('keydown', listener, true)
  return () => target.removeEventListener('keydown', listener, true)
}
