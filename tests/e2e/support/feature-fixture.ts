import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { randomUUID } from 'node:crypto';

export function required(name: string): string {
  const value = process.env[name];
  assert.ok(value, `Set ${name} explicitly for the authorized fixture NAS`);
  return value;
}

/** No fallback host or share: a missing configuration must never select a production target. */
export function featureFixture() {
  const sharePath = required('E2E_FEATURE_SHARE');
  assert.match(sharePath, /^\/volume\d+\/reclaim-e2e-[a-z0-9-]+$/);
  const name = `reclaim-e2e-${randomUUID()}`;
  return { sharePath, share: sharePath.split('/').at(-1)!, volume: sharePath.split('/').slice(0, 2).join('/'),
    name, root: `${sharePath}/${name}`, recycled: `${sharePath}/#recycle/${name}` };
}
export type FeatureFixture = ReturnType<typeof featureFixture>;

/** The configured SSH adapter accepts normal ssh argv and sends stdin to a root shell. */
export async function fixtureShell(script: string): Promise<string> {
  const child = spawn(process.env.E2E_SSH_COMMAND || 'ssh',
    ['-o', 'BatchMode=yes', required('DSM_E2E_SSH'), 'sh', '-s'], { stdio: ['pipe', 'pipe', 'pipe'], timeout: 120_000 });
  let stdout = '';
  child.stdout.on('data', chunk => { stdout += chunk; });
  child.stderr.resume();
  child.stdin.end(script);
  const code = await new Promise<number>((resolve, reject) => {
    child.on('error', reject);
    child.on('close', code => resolve(code ?? 1));
  });
  assert.equal(code, 0, 'Owned fixture SSH operation succeeded');
  return stdout.trim();
}

export async function createFeatureFixture(f: FeatureFixture): Promise<void> {
  const owner = required('E2E_FIXTURE_OWNER');
  assert.match(owner, /^[A-Za-z0-9_][A-Za-z0-9_.-]*$/);
  // A dedicated pre-created DSM share is required. Never create #recycle in a user's share.
  await fixtureShell(`set -eu
test -d '${f.sharePath}/#recycle'
# Waiting for completion is allowed only for a tiny share created for this run.
# DSM may create @eaDir metadata when registering the share. Bound it too.
test -z "$(find '${f.sharePath}' -mindepth 1 -maxdepth 1 ! -name '#recycle' ! -name '@eaDir' -print -quit)"
# DSM creates this Explorer icon file when recycling is enabled. Permit only
# its exact default bytes, never a symlink or an arbitrary same-named file.
desktop='${f.sharePath}/#recycle/desktop.ini'
if test -e "$desktop" || test -L "$desktop"; then
  test -f "$desktop" && test ! -L "$desktop"
  printf '%s\\n' '[.ShellClassInfo]' 'IconFile=%SystemRoot%\\system32\\SHELL32.dll' 'IconIndex=32' | cmp - "$desktop"
fi
test -z "$(find '${f.sharePath}/#recycle' -mindepth 1 -maxdepth 1 ! -name '@eaDir' ! -name 'desktop.ini' -print -quit)"
test "$(du -sk '${f.sharePath}' | awk '{print $1}')" -le 1024
mkdir '${f.root}' '${f.recycled}'
head -c 1048576 /dev/urandom > '${f.root}/control.bin'
head -c 2097152 /dev/urandom > '${f.recycled}/discard.bin'
chown -R '${owner}:users' '${f.root}' '${f.recycled}'
sync
`);
}

export async function featureExists(path: string, f: FeatureFixture): Promise<boolean> {
  assert.ok([f.root, f.recycled, `${f.root}/control.bin`, `${f.recycled}/discard.bin`].includes(path));
  return (await fixtureShell(`if test -e '${path}'; then echo yes; else echo no; fi`)) === 'yes';
}

export async function removeFeatureFixture(f: FeatureFixture): Promise<void> {
  await fixtureShell(`set -eu
rm -rf -- '${f.root}' '${f.recycled}'
test ! -e '${f.root}'
test ! -e '${f.recycled}'
`);
}
