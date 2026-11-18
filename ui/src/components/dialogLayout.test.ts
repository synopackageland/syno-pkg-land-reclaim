// Guards the dialog layout rule: the DSM AppWindow is
// 1100x720, so a dialog panel must fit it with the header and footer always
// visible and only the body scrolling. UI 0.9.5's Dialog does this itself:
// the panel is a flex column, the body the one scroll container, and the
// embedded theme caps the panel in the app and beside the DSM window.
import { readdirSync, readFileSync } from 'node:fs'
import { join } from 'node:path'
import { createRequire } from 'node:module'
import { defineComponent, h, nextTick } from 'vue'
import { mount } from '@vue/test-utils'
import ConfirmDialog from './ConfirmDialog.vue'
import { AppRoot } from '@synopackageland/ui'

const embeddedTheme = readFileSync(createRequire(import.meta.url).resolve('@synopackageland/ui/themes/dsm7-embedded.css'), 'utf8')

async function mountDialog() {
  const host = defineComponent({
    render: () => h(AppRoot, {}, {
      default: () => h(ConfirmDialog, {
        open: true,
        title: 'Confirm cleanup',
        checkLabel: 'I understand',
        confirmLabel: 'Delete',
        cancelLabel: 'Close',
        closeLabel: 'Close',
      }, { default: () => h('div', { style: 'height:3000px' }, 'long body') }),
    }),
  })
  const wrapper = mount(host, { attachTo: document.body })
  await nextTick()
  return wrapper
}

function panel() {
  const element = document.querySelector<HTMLElement>('[role="dialog"]')
  if (!element) throw new Error('the dialog was not rendered')
  return element
}

describe('dialog layout', () => {
  afterEach(() => {
    document.body.innerHTML = ''
  })

  it('lays the panel out as a column so the header and footer stay put', async () => {
    const wrapper = await mountDialog()
    expect(panel().classList).toContain('flex')
    expect(panel().classList).toContain('flex-col')
    for (const selector of ['header', 'footer']) {
      expect(panel().querySelector(selector)!.classList).toContain('shrink-0')
    }
    wrapper.unmount()
  })

  // The published UI owns the cascade and measures the floating owner's height
  // Browser acceptance checks actual containment; avoid a snapshot of minified CSS
  it('ships app and floating-owner height bounds in the embedded theme', () => {
    const rules = [...embeddedTheme.matchAll(/([^{}]+)\{([^{}]+)\}/g)]
    const app = rules.find(([, selector, declarations]) => declarations.includes('max-height:') && selector.includes('[data-ui-modal-scope=app] [role=dialog]:not([data-ui-popover]):not([data-ui-sheet])') && !selector.includes('[data-ui-floating]'))?.[2]
    const floating = rules.find(([, selector, declarations]) => declarations.includes('max-height:') && selector.includes('[data-ui-floating][data-ui-modal-scope=app] [role=dialog]'))?.[2]
    expect(app).toBeDefined()
    expect(floating).toBeDefined()
    // 24px, or the kit's --ui-space-6 token for it (UI 0.25.18 and later).
    expect(app).toMatch(/max-height:calc\(100%\s*-\s*(24px|var\(--ui-space-6\))\)!important/)
    expect(floating).toContain('--ui-floating-max-height')
    expect(floating).toContain('--ui-floating-owner-height')
    expect(floating).toMatch(/max-height:min\(/)
  })

  it('scrolls only the body, with the footer outside it', async () => {
    const wrapper = await mountDialog()
    const content = panel()
    const body = content.children[1] as HTMLElement
    const footer = content.querySelector('footer')
    if (!footer) throw new Error('the dialog has no footer')

    expect(body.classList).toContain('overflow-auto')
    expect(body.classList).toContain('min-h-0')
    expect(content.children[2]).toBe(footer)
    expect(body.contains(footer)).toBe(false)
    wrapper.unmount()
  })
})

it('does not reference a missing description when body content explains the confirmation', async () => {
  const wrapper = await mountDialog()
  try {
    const description = panel().getAttribute('aria-describedby')
    expect(description === null || document.getElementById(description) !== null).toBe(true)
  } finally { wrapper.unmount() }
})

// UI 0.10's Dialog sizes to its content between --ui-dialog-min-width and
// --ui-dialog-max-width, as DSM's windows do; a fixed width overrides that.
describe('app dialogs', () => {
  const srcDir = join(__dirname, '..')
  const sources = (readdirSync(srcDir, { recursive: true }) as string[])
    .filter(file => file.endsWith('.vue'))
    .map(file => ({ file, text: readFileSync(join(srcDir, file), 'utf8') }))

  it('size to their content instead of a fixed width', () => {
    const fixed = sources.flatMap(({ file, text }) =>
      [...text.matchAll(/<Dialog\b[^>]*>/g)].filter(([tag]) => /\swidth=/.test(tag)).map(() => file))
    expect(fixed).toEqual([])
  })
})
