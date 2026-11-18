// @vitest-environment node
// Every --ui-* token the App reads must be one the theme, the SDK or the App
// defines: an unknown name silently falls back to its hard-coded value.
import { readdirSync, readFileSync } from 'node:fs'
import { createRequire } from 'node:module'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'

const require = createRequire(import.meta.url)
const srcDir = fileURLToPath(new URL('.', import.meta.url))
const sources = (readdirSync(srcDir, { recursive: true }) as string[])
  .filter(file => /\.(vue|css|ts)$/.test(file) && !file.endsWith('.test.ts'))
  .map(file => ({ file, text: readFileSync(join(srcDir, file), 'utf8') }))
const defined = (text: string) => [...text.matchAll(/(--ui-[\w-]+)\s*:/g)].map(match => match[1])
const known = new Set([
  ...defined(readFileSync(require.resolve('@synopackageland/ui/themes/dsm7-embedded.css'), 'utf8')),
  ...defined(readFileSync(require.resolve('@synopackageland/sdk/vue/style.css'), 'utf8')),
  ...sources.flatMap(({ text }) => defined(text)),
])

it('reads only --ui-* tokens that are defined', () => {
  const unknown = sources.flatMap(({ file, text }) =>
    [...text.matchAll(/var\((--ui-[\w-]+)/g)].map(match => match[1]).filter(name => !known.has(name)).map(name => `${file}: ${name}`))
  expect([...new Set(unknown)]).toEqual([])
})

// DSM's corners come from the UI's radius scale (--ui-radius-sm/md/lg/pill);
// a hard-coded radius drifts from it. 2px colour chips and circles stay literal.
it('rounds corners with the UI radius tokens', () => {
  const literal = sources.flatMap(({ file, text }) =>
    [...text.matchAll(/border-radius:\s*([^;}]+)/g)].map(match => match[1].trim())
      .filter(value => /\b(?:[3-9]|\d{2,})px\b/.test(value)).map(value => `${file}: ${value}`))
  expect(literal).toEqual([])
})
