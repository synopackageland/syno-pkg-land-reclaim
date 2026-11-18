// @vitest-environment node
// Every fixture must be a document the CLI can print.
import { readdirSync, readFileSync, existsSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import Ajv2020 from 'ajv/dist/2020'
import type { FixtureEntry } from './fixtures'
import type { Method } from './methods'

const fixtureDir = fileURLToPath(new URL('../../fixtures/', import.meta.url))
const schemaDir = fileURLToPath(new URL('../../../schemas/v1/', import.meta.url))

// Planning folders hold recorded worker result documents; fixtureBackend wraps
// them in asynchronous jobs, matching the real App API.
// Method → the schema(s) its recorded document may follow; every method may also
// answer with a webapi refusal.
const SCHEMAS: Partial<Record<Method, string[]>> = {
  'session.get': ['session'],
  'volumes.list': ['volumes'],
  'jobs.list': ['jobs'],
  'history.list': ['history'],
  'overview.get': ['overview'],
  'top.list': ['top'],
  'inspect.get': ['inspect'],
  'simulate.get': ['simulate-delete'],
  'snapshots.list': ['snapshots'],
  'why_not_freed.get': ['why-not-freed'],
  'recycle.list': ['recycle'],
  'cleanup.plan': ['cleanup-plan'],
  'watch_free.sample': ['watch-free'],
  'scan.start': ['detach'],
  'cleanup.apply': ['detach', 'cleanup-apply'],
  'job.resume': ['detach', 'resume'],
  'job.pause': ['pause'],
  'job.mode': ['mode'],
  'job.cancel': ['cancel'],
  'job.result': ['scan', 'cleanup-apply', 'rootfs-apply', 'job-result'],
  'rootfs.report': ['rootfs'],
  'rootfs.plan': ['rootfs-plan'],
  'rootfs.apply': ['detach'],
  'rootfs.history': ['rootfs-history'],
  'cache.status': ['cache'],
  'cache.clear': ['cache'],
}

const ajv = new Ajv2020({ strict: false, allErrors: true })
for (const file of readdirSync(schemaDir)) ajv.addSchema(JSON.parse(readFileSync(schemaDir + file, 'utf8')), file)

const readJson = (path: string) => JSON.parse(readFileSync(path, 'utf8'))
const methods = readdirSync(fixtureDir, { withFileTypes: true }).filter(d => d.isDirectory()).map(d => d.name)

describe.each(methods)('fixtures of %s', method => {
  const entries: FixtureEntry[] = readJson(`${fixtureDir}${method}/index.json`)

  it.each(entries.filter(e => e.file).map(e => e.file!))('%s exists and follows its schema', file => {
    const path = `${fixtureDir}${method}/${file}`
    expect(existsSync(path)).toBe(true)
    const document = readJson(path)
    if (method === 'jobs.list' && file.startsWith('review-cleanup-')) {
      // These fixtures follow the cleanup-apply job contract added by !68;
      // this branch's base jobs schema predates that upstream change.
      const job = readJson(path).jobs[0]
      expect(job).toMatchObject({ kind: 'cleanup-apply', phase: 'execute' })
      expect(job).not.toHaveProperty('progress')
      expect(job).not.toHaveProperty('cleanup_result')
      if (file.endsWith('completed.json')) {
        expect(job.cleanup).toMatchObject({
          items_total: expect.any(Number),
          items_done: expect.any(Number),
          moved: expect.any(Number),
          already_absent: expect.any(Number),
          partial: expect.any(Number),
          failed: expect.any(Number),
          changed: expect.any(Number),
          bytes_total: expect.any(Number),
          bytes_moved: expect.any(Number),
          elapsed_s: expect.any(Number),
          updated: expect.any(String),
        })
      } else {
        expect(job.state).toBe('failed')
        expect(job.result.completion).toBe('failed')
      }
      return
    }
    const names = [...(SCHEMAS[method as Method] ?? []), 'webapi']
    const valid = names.some(name => ajv.validate(`${name}.schema.json`, document))
    expect(valid, `${method}/${file} matches none of ${names.join(', ')}`).toBe(true)
  })
})

describe('Recycle Bin edge fixtures', () => {
  it('keeps the captured oldest-item sample aligned with the share aggregate', () => {
    const volume = readJson(`${fixtureDir}recycle.list/volume1.json`)
    const oldest200 = readJson(`${fixtureDir}recycle.list/share-live-oldest200.json`).shares[0]
    const oldest1000 = readJson(`${fixtureDir}recycle.list/share-live-oldest1000.json`).shares[0]
    const sharePath = (value: string | { path: string }) => typeof value === 'string' ? value : value.path
    const share = volume.shares.find(
      (item: { share: string | { path: string } }) => sharePath(item.share) === sharePath(oldest1000.share),
    )

    expect(share).toBeDefined()
    expect(sharePath(oldest200.share)).toBe(sharePath(oldest1000.share))
    expect(oldest1000).toMatchObject({
      items: share.items,
      bytes: share.bytes,
      reclaimable_bytes: share.reclaimable_bytes,
    })
    expect(oldest1000.oldest).toHaveLength(6)
    expect(oldest1000.oldest).toEqual(oldest200.oldest)
    expect(oldest1000.oldest.length).toBeLessThan(share.items)
    expect(oldest200.oldest).toEqual(oldest1000.oldest.slice(0, 200))
  })
})
