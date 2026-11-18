import assert from 'node:assert/strict';
import { mkdtemp, mkdir, writeFile, readFile, rm, symlink } from 'node:fs/promises';
import { tmpdir, userInfo } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { createFeatureFixture, removeFeatureFixture } from './feature-fixture.ts';

const desktop = '[.ShellClassInfo]\nIconFile=%SystemRoot%\\system32\\SHELL32.dll\nIconIndex=32\n';

async function withFixture(run: (root: string, fixture: Parameters<typeof createFeatureFixture>[0]) => Promise<void>) {
  const scratch = await mkdtemp(join(tmpdir(), 'reclaim-fixture-test-'));
  const root = join(scratch, 'share');
  await mkdir(root);
  const previous = { ...process.env };
  const sharePath = '/volume1/reclaim-e2e-harness-test';
  const fixture = { sharePath, share: 'reclaim-e2e-harness-test', volume: '/volume1', name: 'reclaim-e2e-child',
    root: `${sharePath}/reclaim-e2e-child`, recycled: `${sharePath}/#recycle/reclaim-e2e-child` };
  try {
    await mkdir(join(root, '#recycle'));
    const adapter = join(scratch, 'ssh-adapter');
    await writeFile(adapter, `#!/usr/bin/env node
const {readFileSync} = require('node:fs');
const {spawnSync} = require('node:child_process');
const script = readFileSync(0, 'utf8').replaceAll(${JSON.stringify(sharePath)}, ${JSON.stringify(root)});
const r = spawnSync('sh', ['-s'], {input: 'sync() { :; }; chown() { :; };\\n' + script});
process.stdout.write(r.stdout); process.stderr.write(r.stderr); process.exit(r.status ?? 1);
`, { mode: 0o700 });
    process.env.E2E_SSH_COMMAND = adapter;
    process.env.DSM_E2E_SSH = 'local-fixture-only';
    process.env.E2E_FIXTURE_OWNER = userInfo().username;
    await run(root, fixture);
  } finally {
    process.env = previous;
    await rm(scratch, { recursive: true, force: true });
  }
}

test('a fresh DSM recycle desktop.ini permits an owned fixture and remains after teardown', async () => {
  await withFixture(async (root, fixture) => {
    await writeFile(join(root, '#recycle/desktop.ini'), desktop);
    await createFeatureFixture(fixture);
    assert.equal((await readFile(join(root, 'reclaim-e2e-child/control.bin'))).length, 1048576);
    await removeFeatureFixture(fixture);
    assert.equal(await readFile(join(root, '#recycle/desktop.ini'), 'utf8'), desktop);
  });
});

for (const kind of ['user-file', 'changed-desktop', 'symlink-desktop'] as const) {
  test(`fixture refuses pre-existing recycle data: ${kind}`, async () => {
    await withFixture(async (root, fixture) => {
      if (kind === 'user-file') await writeFile(join(root, '#recycle/user.txt'), 'keep');
      if (kind === 'changed-desktop') await writeFile(join(root, '#recycle/desktop.ini'), 'user data');
      if (kind === 'symlink-desktop') {
        await mkdir(join(root, '#recycle/@eaDir'));
        await writeFile(join(root, '#recycle/@eaDir/target.ini'), desktop);
        await symlink('@eaDir/target.ini', join(root, '#recycle/desktop.ini'));
      }
      await assert.rejects(createFeatureFixture(fixture), /Owned fixture SSH operation succeeded/);
    });
  });
}
