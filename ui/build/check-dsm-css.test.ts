import { readFileSync } from 'node:fs'
import { createRequire } from 'node:module'
import { APP_ROOT_SELECTOR } from '@synopackageland/ui'
import { dsmCssProblems, dsmScrollProblems, ROOT } from './check-dsm-css'

const reset = '[data-ui-app] :where(:not(svg,svg *)){margin:revert}'
const base = '[data-ui-app],[data-ui-app] :where(*),[data-ui-app] :where(*):before,[data-ui-app] :where(*):after{border:0 solid}'
const theme = '[data-ui-app],:where([data-ui-app],[data-ui-app] *)[data-ui-theme=dsm7]{--ui-primary:#0086e5}'
const utility = ':where([data-ui-app],[data-ui-app] *).flex{display:flex}'

describe('dsmCssProblems', () => {
  it('uses the installed shared border preflight without app defaults', () => {
    const embedded = createRequire(import.meta.url).resolve('@synopackageland/ui/themes/dsm7-embedded.css')
    expect(dsmCssProblems(readFileSync(embedded, 'utf8'))).toEqual([])
  })

  it('checks the root of the library AppRoot', () => {
    expect(ROOT).toBe(APP_ROOT_SELECTOR)
  })

  it('accepts the shared preflight before the embedded reset and theme', () => {
    expect(dsmCssProblems(base + reset + theme + utility)).toEqual([])
  })

  it('reports a bundle without the theme tokens', () => {
    expect(dsmCssProblems(base + reset + utility)).toEqual(['no theme tokens on [data-ui-app]'])
  })

  it('reports tokens left on :root, which would restyle the DSM desktop', () => {
    expect(dsmCssProblems(base + reset + theme + ':root{--ui-primary:#0086e5}' + utility)).toEqual(['theme tokens on :root'])
  })

  it('reports cascade layers, which lose to every DSM rule', () => {
    expect(dsmCssProblems(base + reset + theme + '@layer utilities{' + utility + '}')).toEqual(['@layer left in the bundle'])
  })

  it('reports a reset missing or after the theme, where it would beat library classes', () => {
    expect(dsmCssProblems(base + theme + utility)).toContain('no DSM element reset before the theme')
    expect(dsmCssProblems(base + theme + reset + utility)).toContain('no DSM element reset before the theme')
  })

  it('reports a shared border preflight missing or after the theme', () => {
    const problem = 'no shared border preflight before the embedded theme'
    expect(dsmCssProblems(reset + theme + utility)).toEqual([problem])
    expect(dsmCssProblems(reset + theme + base + utility)).toEqual([problem])
    expect(dsmCssProblems(reset + theme + utility + base)).toEqual([problem])
  })
})

// The AppWindow is a fixed 720 px (the window `synopkgland build` generates) and every page is
// taller, so the content area must be the scroll container.
const scroll = '.app-shell[data-v-1] main{overflow-y:auto}'

describe('dsmScrollProblems', () => {
  it('accepts the app content area as the scroll container', () => {
    expect(dsmScrollProblems(base + reset + theme + scroll)).toEqual([])
  })

  it('accepts a flex main with a shrinking page scroll container below banners', () => {
    expect(dsmScrollProblems('.app-shell[data-v-1] main{display:flex;flex-direction:column;overflow:hidden}.app-page[data-v-1]{flex:1 1 auto;min-height:0;overflow-y:auto}')).toEqual([])
  })

  it('does not accept a page wrapper that cannot shrink below banners', () => {
    expect(dsmScrollProblems('.app-shell[data-v-1] main{display:flex;flex-direction:column}.app-page[data-v-1]{overflow-y:auto}')).toHaveLength(1)
  })

  it('reports a bundle that leaves the content area clipped', () => {
    expect(dsmScrollProblems(base + reset + theme + utility)).toEqual([
      'no scroll container for the app content (main or the SDK shell .app-page)',
    ])
  })

  it('does not count a library rule that only sets min-height', () => {
    expect(dsmScrollProblems(base + reset + theme + '.app-shell[data-v-1] main{min-height:0}')).toHaveLength(1)
  })
})
