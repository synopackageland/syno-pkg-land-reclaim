import { mount } from '@vue/test-utils'
import { AppRoot, Dialog } from '@synopackageland/ui'
import { defineComponent, h, nextTick, ref, type Ref } from 'vue'
import { installEscapeGuard } from './useEscapeGuard'

/** An app root holding one real dialog whose open state is `open`. */
function dialogHost(open: Ref<boolean>, dialogBounds: 'app' | 'viewport' = 'app') {
  return defineComponent({
    render: () => h(AppRoot, { dialogBounds }, { default: () => h(Dialog, { open: open.value, title: 'Confirm', description: 'Confirm this action', 'onUpdate:open': (value: boolean) => { open.value = value } }) }),
  })
}

describe('installEscapeGuard', () => {
  afterEach(() => {
    document.body.innerHTML = ''
  })

  it('keeps Escape away from the AppWindow and lets the dialog still close', () => {
    const appWindow = document.createElement('div')
    const root = document.createElement('div')
    const dialog = document.createElement('div')
    dialog.setAttribute('role', 'dialog')
    root.appendChild(dialog)
    appWindow.appendChild(root)
    document.body.appendChild(appWindow)
    let appWindowSawEscape = false
    let dialogClosed = false
    // The DSM AppWindow reacts on the bubble phase of its own element.
    appWindow.addEventListener('keydown', () => { appWindowSawEscape = true })
    // reka-ui's dialog dismissal listens on the window bubble phase.
    window.addEventListener('keydown', () => { dialogClosed = true })

    const uninstall = installEscapeGuard(root, document)
    const pressed = new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true })
    dialog.dispatchEvent(pressed)

    expect(appWindowSawEscape).toBe(false)
    expect(dialogClosed).toBe(true)
    expect(pressed.defaultPrevented).toBe(true)
    uninstall()
  })

  it('leaves Escape in another app or window untouched', () => {
    const root = document.createElement('div')
    const dialog = document.createElement('div')
    dialog.setAttribute('role', 'dialog')
    root.appendChild(dialog)
    document.body.appendChild(root)

    const otherApp = document.createElement('div')
    const otherDialog = document.createElement('div')
    otherDialog.setAttribute('role', 'dialog')
    otherApp.appendChild(otherDialog)
    document.body.appendChild(otherApp)
    let otherSawEscape = false
    otherApp.addEventListener('keydown', () => { otherSawEscape = true })

    const uninstall = installEscapeGuard(root, document)
    const pressed = new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true })
    otherDialog.dispatchEvent(pressed)

    expect(otherSawEscape).toBe(true)
    expect(pressed.defaultPrevented).toBe(false)
    uninstall()
  })

  it('only intercepts Escape while one of this app\'s dialogs is open, and stops after uninstall', () => {
    const root = document.createElement('div')
    document.body.appendChild(root)
    let seen = 0
    const handler = () => { seen += 1 }
    root.addEventListener('keydown', handler)
    const uninstall = installEscapeGuard(root, document)

    root.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }))
    expect(seen).toBe(1)

    const dialog = document.createElement('div')
    dialog.setAttribute('role', 'dialog')
    root.appendChild(dialog)
    seen = 0
    root.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }))
    expect(seen).toBe(0)

    uninstall()
    seen = 0
    root.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }))
    expect(seen).toBe(1)
  })

  // The library's own Dialog dismissal listens on `window` (reka-ui), so the
  // guard's re-dispatched Escape still closes the real dialog while the DSM
  // AppWindow that wraps the app never sees the key (0.4.1 upgrade).
  it('closes a real dialog inside the app without letting the AppWindow see Escape', async () => {
    const appWindow = document.createElement('div')
    document.body.appendChild(appWindow)
    let appWindowSawEscape = false
    appWindow.addEventListener('keydown', () => { appWindowSawEscape = true })
    const open = ref(true)
    const wrapper = mount(dialogHost(open), { attachTo: appWindow })
    await nextTick()
    const uninstall = installEscapeGuard(appWindow, document)

    const dialog = appWindow.querySelector<HTMLElement>('[role="dialog"]')
    expect(dialog).not.toBeNull()
    dialog!.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }))
    await nextTick()

    expect(open.value).toBe(false)
    expect(appWindowSawEscape).toBe(false)
    uninstall()
    wrapper.unmount()
  })

  // In a DSM window the dialog floats in a host beside the window element
  // (dialog-bounds="viewport"); Escape there still closes it and never
  // reaches the window.
  it('closes a dialog that floats outside the AppWindow without letting the window see Escape', async () => {
    const appWindow = document.createElement('div')
    appWindow.style.zIndex = '10'
    document.body.appendChild(appWindow)
    let appWindowSawEscape = false
    appWindow.addEventListener('keydown', () => { appWindowSawEscape = true })
    const open = ref(true)
    const wrapper = mount(dialogHost(open, 'viewport'), { attachTo: appWindow })
    await nextTick()
    await nextTick()
    const uninstall = installEscapeGuard(appWindow, document)

    const dialog = document.querySelector<HTMLElement>('[data-ui-floating] [role="dialog"]')
    expect(dialog).not.toBeNull()
    expect(appWindow.contains(dialog)).toBe(false)
    dialog!.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }))
    await nextTick()

    expect(open.value).toBe(false)
    expect(appWindowSawEscape).toBe(false)
    uninstall()
    wrapper.unmount()
  })
})
