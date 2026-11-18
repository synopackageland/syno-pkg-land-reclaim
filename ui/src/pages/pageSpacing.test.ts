// @vitest-environment node
// UI 0.10's PageContent pads the page and spaces its sections with
// --ui-section-gap; the SDK's AppShell mounts every page inside it. A page
// that pads itself or picks its own gap doubles or skews that spacing.
import { readdirSync, readFileSync } from 'node:fs'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'

const pagesDir = fileURLToPath(new URL('.', import.meta.url))
const read = (file: string) => readFileSync(join(pagesDir, file), 'utf8')
const pageFiles = (readdirSync(pagesDir, { recursive: true }) as string[]).filter(file => file.endsWith('.vue'))
const rules = (file: string, selector: string) =>
  [...read(file).matchAll(new RegExp(`(?:^|[{}]\\s*)${selector.replace('.', '\\.')}\\s*\\{([^}]*)\\}`, 'gm'))].map(match => match[1])
const rule = (file: string, selector: string) => rules(file, selector)[0] ?? ''

it('leaves the page padding to PageContent', () => {
  expect(pageFiles.filter(file => /--ui-page-padding-[xy]/.test(read(file)))).toEqual([])
})

it.each([
  ['Overview/OverviewPage.vue', '.overview'],
  ['History/HistoryPage.vue', '.history-page'],
  ['Recycle/RecyclePage.vue', '.recycle-page'],
  ['Snapshots/SnapshotsPage.vue', '.snapshot-page'],
  ['Packages/PackagesPage.vue', '.body'],
  ['Scanning/ScanningPage.vue', '.scanning-page'],
])('%s stacks its sections at the UI section gap', (file, selector) => {
  expect(rule(file, selector)).toMatch(/gap:\s*var\(--ui-section-gap\)/)
  expect(rules(file, selector).filter(body => /padding/.test(body))).toEqual([])
})

it('Explore stacks its sections at the UI section gap without its own padding', () => {
  const body = read('Explore/ExplorePage.vue').match(/<div class="(flex min-h-0 flex-1 flex-col[^"]*)">/)?.[1] ?? ''
  expect(body).toContain('gap-[var(--ui-section-gap)]')
  expect(body).not.toMatch(/(^|\s)p-\d/)
})
