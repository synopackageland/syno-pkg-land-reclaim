import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createRequire } from 'node:module';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { required } from './feature-fixture.ts';

const require = createRequire(import.meta.url);
const Ajv = require('ajv');
const validator = new Ajv({ allErrors: true, strict: false });
const descriptor = JSON.parse(readFileSync(resolve(import.meta.dirname, '../../../generated/synopkgland-api.json'), 'utf8'));
const cli = resolve(import.meta.dirname, '../../../node_modules/@synopackageland/cli/dist/bin.js');

/** Spawn the installed CLI, including real discovery, DSM authentication and schema validation. */
export async function ctl(id: string, input: Record<string, unknown> = {}): Promise<any> {
  const operation = descriptor.payload.commands[id];
  assert.ok(operation, `Unknown installed operation ${id}`);
  assert.ok(validator.validate(operation.paramsSchema, input), `${id}: test input violates the pinned ctl schema`);
  const args = [cli, 'ctl', 'reclaim', ...operation.cli, '--host', required('E2E_CTL_HOST'), '--json', '--input', '-'];
  if (operation.effect !== 'read') args.push('--yes');
  if (operation.flow.type === 'job') args.push('--no-wait');
  const child = spawn(process.execPath, args, { stdio: ['pipe', 'pipe', 'pipe'], timeout: 120_000 });
  let stdout = '';
  child.stdout.on('data', chunk => { stdout += chunk; });
  // CLI diagnostics can contain local host metadata. The assertion below exposes only safe codes.
  child.stderr.resume();
  child.stdin.end(JSON.stringify(input));
  const code = await new Promise<number>((resolve, reject) => {
    child.on('error', reject);
    child.on('close', code => resolve(code ?? 1));
  });
  let envelope;
  try { envelope = JSON.parse(stdout); } catch { assert.fail(`${id}: CLI returned no JSON (exit ${code})`); }
  if (process.env.E2E_CTL_EVIDENCE_DIR) {
    mkdirSync(process.env.E2E_CTL_EVIDENCE_DIR, { recursive: true, mode: 0o700 });
    writeFileSync(resolve(process.env.E2E_CTL_EVIDENCE_DIR, `${id}.json`), JSON.stringify(envelope), { mode: 0o600 });
  }
  assert.equal(envelope.ok, true, `${id}: ${envelope.error?.code ?? 'CLI request refused'}`);
  assert.equal(code, 0, `${id}: CLI exit code`);
  assert.ok(validator.validate(operation.resultSchema, envelope.data),
    `${id}: result violates the pinned schema (${validator.errors?.map((e: { keyword: string }) => e.keyword).join(', ')})`);
  if (operation.flow.type === 'job') {
    assert.ok(envelope.data.job, `${id}: asynchronous admission is present`);
    return envelope.data.job;
  }
  return envelope.data;
}

export async function waitForTinyShareScan(jobId: string, sharePath: string): Promise<any> {
  assert.match(sharePath, /^\/volume\d+\/reclaim-e2e-[a-z0-9-]+$/, 'Only a tiny owned share may be awaited to completion');
  const until = Date.now() + 15 * 60_000;
  while (Date.now() < until) {
    const status = await ctl('scans.status', { job_id: jobId });
    assert.equal(status.job_id, jobId, 'Status keeps the original product ID');
    if (['completed', 'partial', 'failed', 'cancelled', 'interrupted', 'paused'].includes(status.state)) {
      assert.equal(status.state, 'completed', `Scan ended ${status.state}`);
      return status;
    }
    await new Promise(resolve => setTimeout(resolve, 2000));
  }
  assert.fail('Scan did not finish within the observation window; inspect the original job ID');
}

/** Teardown cancels unfinished work. Only tiny owned share scans may wait for completion. */
export async function cancelCtlScan(jobId: string): Promise<void> {
  const terminal = ['completed', 'partial', 'failed', 'cancelled', 'interrupted', 'superseded'];
  if (terminal.includes((await ctl('scans.status', { job_id: jobId })).state)) return;
  await ctl('ctl.job.cancel', { job_id: jobId });
  const until = Date.now() + 120_000;
  while (Date.now() < until) {
    const status = await ctl('scans.status', { job_id: jobId });
    if (terminal.includes(status.state)) {
      assert.equal(status.state, 'cancelled', 'Unfinished owned scan was cancelled');
      return;
    }
    await new Promise(resolve => setTimeout(resolve, 1000));
  }
  assert.fail('Owned scan did not cancel; retain fixture for investigation');
}
