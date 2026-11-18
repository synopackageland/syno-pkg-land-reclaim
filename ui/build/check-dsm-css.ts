// Checks the DSM bundle's stylesheet (`pnpm check:dsm` after `pnpm build:dsm`):
// what the DSM window needs from the order src/styles.ts loads the library's
// shared embedded reset and theme in, which the dev page cannot show
// because it has no DSM CSS.
import { readFileSync } from 'node:fs'

// The root the library's AppRoot renders (its APP_ROOT_SELECTOR).
export const ROOT = '[data-ui-app]'

const RULE = /([^{}]+)\{([^{}]*)\}/g
const RESET = `${ROOT} :where(:not(svg`
const SHARED_BASE = `${ROOT},${ROOT} :where(*)`

export function dsmCssProblems(css: string): string[] {
  const rules = [...css.matchAll(RULE)].map(m => ({ at: m.index, selector: m[1].trim(), body: m[2] }))
  const tokens = rules.filter(r => r.body.includes('--ui-'))
  const theme = tokens.find(r => r.selector.split(',').some(s => s.trim() === ROOT))
  const reset = rules.find(r => r.selector.startsWith(RESET))
  const base = rules.find(r => r.selector.startsWith(SHARED_BASE) && /(?:^|;)border\s*:\s*0(?:px)?\s+solid(?:;|$)/.test(r.body))
  const problems: string[] = []
  if (!theme) problems.push(`no theme tokens on ${ROOT}`)
  if (tokens.some(r => r.selector.split(',').some(s => s.trim() === ':root'))) problems.push('theme tokens on :root')
  if (css.includes('@layer')) problems.push('@layer left in the bundle')
  if (!reset || (theme && reset.at > theme.at)) problems.push('no DSM element reset before the theme')
  if (!base || (theme && base.at > theme.at)) {
    problems.push('no shared border preflight before the embedded theme')
  }
  return problems
}

/**
 * The DSM AppWindow is a fixed 1100x720 (the window `synopkgland build` generates) and the sidebar
 * is 100 % tall, so the app's content area must be the scroll container or the
 * bottom of every page is unreachable.
 */
export function dsmScrollProblems(css: string): string[] {
  const rules = [...css.matchAll(RULE)].map(m => ({ selector: m[1].trim(), body: m[2] }))
  const scrolls = rules.some(
    r => r.selector.split(',').some(s => s.includes('.app-shell') && /\bmain\b/.test(s)) && /overflow-y\s*:\s*auto/.test(r.body),
  )
  // A banner above a full-height grid must consume space in a flex main;
  // only the SDK shell's shrinking page scrolls, keeping the grid footer visible.
  const flexMain = rules.some(r => r.selector.includes('.app-shell') && /\bmain\b/.test(r.selector)
    && /display\s*:\s*flex/.test(r.body) && /flex-direction\s*:\s*column/.test(r.body))
  const pageScrolls = rules.some(r => r.selector.includes('.app-page')
    && /overflow-y\s*:\s*auto/.test(r.body) && /min-height\s*:\s*0/.test(r.body) && /flex\s*:\s*1/.test(r.body))
  return scrolls || (flexMain && pageScrolls) ? [] : ['no scroll container for the app content (main or the SDK shell .app-page)']
}

if (import.meta.main) {
  const file = process.argv[2] ?? 'dist-dsm/app.css'
  const css = readFileSync(file, 'utf8')
  const problems = [...dsmCssProblems(css), ...dsmScrollProblems(css)]
  for (const problem of problems) console.error(`${file}: ${problem}`)
  process.exit(problems.length ? 1 : 0)
}
