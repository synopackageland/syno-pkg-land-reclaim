// @vitest-environment node
import {mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync} from 'node:fs'
import {tmpdir} from 'node:os'
import {join, resolve} from 'node:path'
import {build, loadConfigFromFile, mergeConfig} from 'vite'
import {expect, test} from 'vitest'

test('the DSM build preserves lazy modules and the SDK stylesheet entry', async () => {
  const work = mkdtempSync(join(tmpdir(), 'reclaim-module-'))
  try {
    writeFileSync(join(work, 'entry.ts'), 'import "./style.css"; export const open = () => import("./lazy.ts")')
    writeFileSync(join(work, 'lazy.ts'), 'export const text = "optional-route-payload"')
    writeFileSync(join(work, 'style.css'), '.bundle-fixture { color: green }')
    const loaded = await loadConfigFromFile({command: 'build', mode: 'dsm'}, resolve('vite.config.ts'))
    expect(loaded).not.toBeNull()
    await build(mergeConfig(loaded!.config, {
      configFile: false,
      logLevel: 'silent',
      build: {outDir: join(work, 'dist'), lib: {entry: join(work, 'entry.ts')}},
    }))
    const entry = readFileSync(join(work, 'dist/app.module.js'), 'utf8')
    expect(entry).toMatch(/import\(["']\.\/chunks\//)
    expect(entry).not.toContain('optional-route-payload')
    const chunks = readdirSync(join(work, 'dist/chunks'))
    expect(chunks).toHaveLength(1)
    expect(readFileSync(join(work, 'dist/chunks', chunks[0]), 'utf8')).toContain('optional-route-payload')
    expect(readFileSync(join(work, 'dist/app.css'), 'utf8')).toContain('bundle-fixture')
  } finally {
    rmSync(work, {recursive: true, force: true})
  }
})
