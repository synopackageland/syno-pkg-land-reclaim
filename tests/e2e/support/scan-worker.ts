import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';

// Keep process start times so cancellation also waits for a worker that has
// closed its lock while exiting; a reused PID is never mistaken for that worker.
const observed = new Map<string, [string, string][]>();

/** Match SDK root.start jobs by their held live.lock and verified environment. */
export async function workerRunning(id: string): Promise<boolean> {
  assert.match(id, /^[a-zA-Z0-9-]+$/);
  const host = process.env.DSM_E2E_SSH;
  assert.ok(host, 'DSM_E2E_SSH must name the authorized acceptance host');
  const child = spawn('ssh', ['-o', 'BatchMode=yes', host, 'python3', '-', id], { stdio: ['pipe', 'pipe', 'pipe'] });
  let output = '', error = '';
  child.stdout.on('data', chunk => { output += chunk; });
  child.stderr.on('data', chunk => { error += chunk; });
  child.stdin.end(`import glob, json, os, sys
tracked = dict(${JSON.stringify(observed.get(host + ':' + id) ?? [])})
data = os.path.realpath('/var/packages/SynoReclaim/var')
state = os.path.join(os.path.dirname(os.path.dirname(data)), '@synopkgland', 'SynoReclaim')
lock = os.path.join(state, 'jobs', sys.argv[1], 'live.lock')
running = []
for path in glob.glob('/proc/[0-9]*'):
    try:
        pid = os.path.basename(path)
        start = open(path + '/stat').read().rsplit(')', 1)[1].split()[19]
        if tracked.get(pid) == start:
            running.append([pid, start])
            continue
        executable = os.path.basename(os.readlink(path + '/exe')).replace(' (deleted)', '')
        if executable not in ('cli', 'synoreclaim'):
            continue
        env = open(path + '/environ', 'rb').read().split(b'\\0')
        if b'SYNOPKGLAND_ROOT_MODE=job' not in env or ('SYNOPKGLAND_ROOT_STATE=' + state).encode() not in env:
            continue
        for fd in glob.glob(path + '/fd/*'):
            try:
                if os.readlink(fd) == lock:
                    running.append([pid, start])
                    break
            except OSError:
                pass
    except OSError:
        continue
print(json.dumps(running))
`);
  const status = await new Promise(resolve => child.on('close', resolve));
  assert.equal(status, 0, error);
  const running: [string, string][] = JSON.parse(output);
  assert.ok(Array.isArray(running) && running.every(row => row.length === 2 && row.every(value => /^\d+$/.test(value))));
  observed.set(host + ':' + id, running);
  return running.length > 0;
}

/** Verify saved files without invoking a CLI command that could recover or rewrite state. */
export async function checkpointExists(id: string, businessRoot?: string): Promise<boolean> {
  assert.match(id, /^[a-zA-Z0-9-]+$/);
  const host = process.env.DSM_E2E_SSH;
  assert.ok(host, 'DSM_E2E_SSH must name the authorized acceptance host');
  if (businessRoot) assert.match(businessRoot, /^\/volume\d+\/[A-Za-z0-9_.-]+\/reclaim-e2e-[a-z0-9-]+$/);
  const child = spawn('ssh', ['-o', 'BatchMode=yes', host, 'python3', '-', id, ...(businessRoot ? [businessRoot] : [])], { stdio: ['pipe', 'pipe', 'pipe'] });
  let output = '', error = '';
  child.stdout.on('data', chunk => { output += chunk; });
  child.stderr.on('data', chunk => { error += chunk; });
  child.stdin.end(`import glob, json, os, subprocess, sys
data = os.path.realpath('/var/packages/SynoReclaim/var')
volume = os.path.dirname(os.path.dirname(data))
root = os.path.join(volume, '@synopkgland', 'SynoReclaim', 'jobs', sys.argv[1], 'checkpoint')
saved = glob.glob(root + '/*/manifest') + glob.glob(root + '/*/namespace*.json')
usable = any(os.path.getsize(path) > 0 for path in saved)
if len(sys.argv) > 2:
    tree = int(subprocess.check_output(['btrfs', 'inspect-internal', 'rootid', sys.argv[2]], text=True).strip())
    keys = []
    for path in saved:
        doc = json.loads(open(path).readline())
        keys.extend([entry['key'] for entry in doc['bundles']] if 'bundles' in doc else [doc['key']])
    usable = any(key['subvolume'] == tree for key in keys)
print('saved' if usable else 'missing')
`);
  const status = await new Promise(resolve => child.on('close', resolve));
  assert.equal(status, 0, error);
  assert.match(output.trim(), /^(saved|missing)$/);
  return output.trim() === 'saved';
}
