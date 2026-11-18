#!/usr/bin/env node
// Reuse inspected release artifacts; the pinned CLI still owns SPK generation.
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { execFileSync } from 'node:child_process'
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { createRequire } from 'node:module'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const require = createRequire(join(root, 'package.json'))
const cli = dirname(require.resolve('@synopackageland/cli/package.json'))
const { x } = require('tar')
const { loadStandaloneManifest } = await import(join(cli, 'dist/standalone/manifest.js'))
const { assembleStandaloneSpk } = await import(join(cli, 'dist/standalone/spk.js'))
const { makeDescriptor } = await import(join(cli, 'dist/ctl/descriptor.js'))
const { sdkSource } = await import(join(cli, 'dist/runtime/sdk-source.js'))
const { portalCatalogs } = await import(join(cli, 'dist/standalone/portal-window.js'))

const [baselineDir, sourceRef] = process.argv.slice(2)
assert(baselineDir && sourceRef, 'Usage: node package/repackage-ui.mjs <trusted-spk-directory> <source-ref>')
const git = (...args) => execFileSync('git', args, { cwd: root, encoding: 'utf8' })
// Refuse runtime, policy, SDK/toolchain or package contract changes. This path
// is only for UI updates against a known, inspected release's exact source.
git('diff', '--exit-code', sourceRef, '--', 'src', 'app', 'build.rs', 'Cargo.toml',
  'Cargo.lock', 'package.json', 'package-lock.json', 'dsm-scripts', 'package/ui', 'package/i18n')
const previous = JSON.parse(git('show', `${sourceRef}:dsm-package.yaml`))
const current = JSON.parse(readFileSync(join(root, 'dsm-package.yaml')))
assert(current.build > previous.build, 'Build must increase')
assert.deepEqual({ ...current, build: previous.build }, previous, 'Only the manifest build may change')
const beforeVersion = `${previous.version}-${previous.build}`
const sha = path => createHash('sha256').update(readFileSync(path)).digest('hex')
// Every file of an unpacked SPK or payload, but the UI the build replaces.
const files = (dir, ...skip) => readdirSync(dir, { recursive: true, withFileTypes: true })
  .filter(entry => entry.isFile())
  .map(entry => join(entry.parentPath, entry.name).slice(dir.length + 1))
  .filter(path => !skip.some(prefix => path === prefix || path.startsWith(`${prefix}/`)))
  .sort()
const unpack = (file, cwd) => x({ file, cwd, strict: true, filter: (name, entry) => {
  assert(!name.startsWith('/') && !name.split('/').includes('..'), 'Unsafe archive path')
  assert(['File', 'Directory'].includes(entry.type), 'Only regular files and directories are supported')
  return true
} })
const work = mkdtempSync(join(tmpdir(), 'synoreclaim-ui-'))
const evidence = []
try {
  // Populate the existing schema-2 root inputs before the manifest check.
  for (const arch of previous.arches) {
    const dir = join(work, arch)
    mkdirSync(dir)
    await unpack(resolve(baselineDir, `${previous.package}-${beforeVersion}-${arch}.spk`), dir)
    assert(readFileSync(join(dir, 'INFO'), 'utf8').includes(`version="${beforeVersion}"`))
    const payload = join(dir, 'payload')
    mkdirSync(payload)
    await unpack(join(dir, 'package.tgz'), payload)
    mkdirSync(join(root, 'root', arch), { recursive: true })
    for (const name of ['cli', 'lifecycle'])
      copyFileSync(join(payload, 'root', name), join(root, 'root', arch, name))
    assert.equal(sha(join(payload, 'app/permissions.json')), sha(join(root, 'app/permissions.json')))
  }
  const loaded = loadStandaloneManifest(root)
  assert(loaded.ok, JSON.stringify(loaded.errors))
  const manifest = loaded.manifest
  mkdirSync(join(root, 'dist'), { recursive: true })
  for (const arch of manifest.arches) {
    const dir = join(work, arch)
    const payload = join(dir, 'payload')
    const setup = JSON.parse(readFileSync(join(payload, 'executor/setup.json')))
    const executor = { ...setup, binary: join(payload, 'executor/executor'),
      lifecycleBinary: join(payload, 'executor/lifecycle'),
      policyText: readFileSync(join(payload, 'executor/policy.json'), 'utf8'), warnings: [] }
    assert.equal(sha(executor.binary), setup.executorDigest)
    assert.equal(sha(join(payload, 'executor/policy.json')), setup.policyDigest)
    const description = JSON.parse(readFileSync(join(payload, 'synopkgland-api.json')))
    description.payload.package.version = manifest.spkVersion
    const metadata = JSON.parse(readFileSync(join(dir, 'sdk-package.json')))
    // The SDK's Hyper Backup control is built with Go only in a full build; reuse it like the binaries.
    const appBackupControl = join(payload, 'backup/control')
    const outputPath = join(root, 'dist', `${manifest.package}-${manifest.spkVersion}-${arch}.spk`)
    await assembleStandaloneSpk({ manifest: { ...manifest, executor }, arch, outputPath,
      cgiPath: join(payload, 'ui/cgi/api.cgi'), app: join(payload, 'app'), executor,
      portalText: portalCatalogs(sdkSource(executor.sdk).goDir),
      apiDescription: makeDescriptor(description.payload), apiRegistry: metadata.ctl.registry,
      ...(existsSync(appBackupControl) ? { appBackupControl } : {}) })
    const check = join(dir, 'check')
    mkdirSync(check)
    await unpack(outputPath, check)
    const packed = join(check, 'payload')
    mkdirSync(packed)
    await unpack(join(check, 'package.tgz'), packed)
    // Nothing the release had may go missing, such as the Hyper Backup control and scripts.
    assert.deepEqual(files(check, 'payload', 'package.tgz'), files(dir, 'payload', 'package.tgz', 'check'), 'SPK files differ from the baseline')
    assert.deepEqual(files(packed, 'ui'), files(payload, 'ui'), 'Payload files differ from the baseline')
    assert.deepEqual(JSON.parse(readFileSync(join(check, 'sdk-package.json'))).hyperBackup, metadata.hyperBackup, 'Hyper Backup record changed')
    const binaries = ['app/bin/synoreclaim-app', 'root/cli', 'root/lifecycle',
      'ui/cgi/api.cgi', 'executor/executor', 'executor/lifecycle', 'executor/policy.json', 'app/permissions.json',
      ...['backup/control', 'backup/config.json'].filter(path => existsSync(join(payload, path)))]
    for (const path of binaries) {
      const digest = sha(join(payload, path))
      assert.equal(sha(join(packed, path)), digest, `Prebuilt artifact changed: ${path}`)
      evidence.push({ arch, path, sha256: digest, unchanged: true })
    }
  }
  console.log(JSON.stringify({ version: manifest.spkVersion, rustCompiled: false, artifacts: evidence }, null, 2))
} finally {
  rmSync(work, { recursive: true, force: true })
}
