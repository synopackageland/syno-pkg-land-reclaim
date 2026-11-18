import { createFixture, fixtureFor, removeFixture, rewriteBusinessFixture } from './support/reclaim.ts';
// Written NAS acceptance coverage. Run only with an explicitly authorized
// registered CLI host. Whole-volume scans are always cancelled after short progress checks.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { test } from '@e2e-dev/web';
import { checkpointExists, workerRunning } from './support/scan-worker.ts';
import { job, openReclaim, poll, terminalStates } from './support/reclaim.ts';
import { cancelScan, growing, pausedProgressStops, startVolumeScan } from './support/scan-lifecycle.ts';

async function ctl(args: string[]): Promise<string> {
  const host = process.env.E2E_CTL_HOST;
  assert.ok(host, 'E2E_CTL_HOST must name the registered, authorized acceptance host');
  const executable = new URL('../../node_modules/@synopackageland/cli/dist/bin.js', import.meta.url);
  const child = spawn(process.execPath, [executable.pathname, 'ctl', 'reclaim', ...args, '--host', host, '--json'], {
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let output = '', error = '';
  child.stdout.on('data', chunk => { output += chunk; });
  child.stderr.on('data', chunk => { error += chunk; });
  const status = await new Promise(resolve => child.on('close', resolve));
  assert.equal(status, 0, `ctl ${args.join(' ')} failed: ${error} ${output}`);
  return output;
}

test('ctl pause/resume starts the original scan after business writes; mode applies both ways; cancel stops it', { session: 'dsm', timeout: 15 * 60_000 }, async ({ app }) => {
  const root = await openReclaim(app);
  const business = fixtureFor('resume-ctl');
  assert.ok(!process.env.E2E_VOLUME || process.env.E2E_VOLUME === '/volume1', 'business fixture must be on the selected volume');
  try {
    await createFixture(business);
    const started = await startVolumeScan(root);
    const id = started.job_id;
    try {
      await growing(id);
      assert.equal(await workerRunning(id), true);
      await ctl(['jobs', 'pause', '--job-id', id, '--yes']);
      const paused = await poll(async () => {
        const current = await job(id);
        assert.ok(!terminalStates.includes(current?.state), `pause failed: ${current?.state}`);
        return current?.state === 'paused' ? current : undefined;
      }, 'ctl pause to finish saving its checkpoint', 120_000, 500);
      assert.equal(await checkpointExists(id), true);
      assert.equal(await checkpointExists(id, business.root), true, 'fixture share must be among saved units; choose an early E2E_FIXTURE_SHARE');
      await pausedProgressStops(id, paused);
      await rewriteBusinessFixture(business);
      const accepted = await ctl(['jobs', 'resume', '--job-id', id, '--yes', '--no-wait']);
      assert.ok(accepted.includes(id), 'ctl returns the original scan ID');
      const resumed = await poll(async () => {
        const current = await job(id);
        assert.ok(!terminalStates.includes(current?.state), `resume failed: ${current?.state}`);
        return current?.state === 'running' ? current : undefined;
      }, 'ctl resume to start its worker', 60_000, 300);
      assert.equal(resumed.job_id, id);
      assert.ok(resumed.resumes > paused.resumes);
      assert.ok((resumed.progress?.items ?? 0) >= (paused.progress?.items ?? 0));
      await growing(id, paused);
      assert.equal(await workerRunning(id), true);
      for (const [mode, profile] of [['gentle', 'low'], ['fast', 'fast']]) {
        await ctl(['jobs', 'mode', '--job-id', id, '--mode', mode, '--yes']);
        await poll(async () => {
          const current = await job(id);
          assert.equal(current?.state, 'running');
          assert.equal(current?.resumes, resumed.resumes, 'mode never restarts the worker');
          return current?.profile === profile || undefined;
        }, `ctl mode ${mode} readback`, 60_000, 500);
      }
      await ctl(['jobs', 'cancel', '--job-id', id, '--yes']);
      const ended = await poll(async () => {
        const current = await job(id);
        return terminalStates.includes(current?.state) ? current : undefined;
      }, 'ctl cancellation', 120_000, 500);
      assert.equal(ended.state, 'cancelled');
      await poll(async () => !(await workerRunning(id)) || undefined, 'ctl cancelled worker to exit', 30_000, 1000);
      const stopped = await ctl(['jobs', 'list']);
      assert.ok(stopped.includes(id) && stopped.includes('cancelled'));
      const replacement = await startVolumeScan(root);
      try {
        assert.notEqual(replacement.job_id, id);
        await growing(replacement.job_id);
      } finally { await cancelScan(replacement.job_id); }
    } finally {
      await cancelScan(id);
    }
  } finally {
    await removeFixture(business);
  }
});
