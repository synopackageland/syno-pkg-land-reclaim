import assert from 'node:assert/strict';
import { api, goTo, job, openReclaim, poll, t, terminalStates } from './reclaim.ts';
import { workerRunning } from './scan-worker.ts';

/** Start the selected test volume through the UI; never wait for its full walk. */
export async function startVolumeScan(root: Awaited<ReturnType<typeof openReclaim>>): Promise<any> {
  await goTo(root, 'overview.title');
  const volumes = (await api('volumes.list')).volumes;
  const volume = process.env.E2E_VOLUME
    ? volumes.find((entry: any) => entry.mount.path === process.env.E2E_VOLUME) : volumes[0];
  assert.ok(volume?.metadata_access, 'acceptance requires a volume with readable metadata');
  const name = volume.mount.path.split('/').at(-1);
  const before = new Set((await api('jobs.list')).jobs.map((entry: any) => entry.job_id));
  await root.getByRole('button', { name: t('shell.newScan'), exact: true }).click();
  const dialog = root.page().getByRole('dialog', { name: t('newScan.title') });
  const choice = dialog.getByRole('radiogroup', { name: t('newScan.volume.title') });
  if (await choice.isVisible()) await choice.getByRole('radio', { name: new RegExp('^' + name + '(?:\\s|$)') }).check();
  else {
    const dropdown = dialog.getByRole('combobox', { name: t('newScan.volume.title') });
    if (await dropdown.isVisible()) {
      await dropdown.click();
      await root.page().getByRole('option', { name: new RegExp('^' + name + '(?:\\s|$)') }).click();
    } else {
      await dialog.getByPlaceholder(t('common.picker.search')).fill(name);
      await dialog.getByRole('radio', { name: new RegExp(`^${name}\\s`) }).check();
    }
  }
  await dialog.getByRole('radio', { name: new RegExp('^' + t('newScan.scope.volume') + '(?:\\s|$)') }).check();
  await dialog.getByRole('radio', { name: new RegExp('^' + t('newScan.mode.fast') + '(?:\\s|$)') }).check();
  await dialog.getByRole('button', { name: t('newScan.startVolume', { volume: name }), exact: true }).click();
  return poll(async () => (await api('jobs.list')).jobs.find((entry: any) => !before.has(entry.job_id) && entry.kind === 'scan'),
    'new volume scan', 30_000, 500);
}

export function counters(scan: any): number[] {
  return ['entries', 'items', 'bytes_read'].map(key => scan.progress?.[key] ?? 0);
}

/** Require monotonic, nonzero progress over multiple polls, keeping the original job. */
export async function growing(id: string, baseline?: any): Promise<any> {
  const first = baseline ?? await poll(async () => {
    const scan = await job(id);
    assert.ok(!terminalStates.includes(scan?.state), `scan ended before progress: ${scan?.state}`);
    return scan?.state === 'running' && scan.progress?.items > 0 ? scan : undefined;
  }, 'initial scan progress', 120_000, 500);
  let previous = counters(first), increases = 0;
  return poll(async () => {
    const scan = await job(id);
    assert.equal(scan?.job_id, id);
    assert.equal(scan?.state, 'running');
    const now = counters(scan);
    now.forEach((value, index) => assert.ok(value >= previous[index], 'progress must not reset'));
    if (now.some((value, index) => value > previous[index])) increases++;
    previous = now;
    return increases >= 2 && now[1] > counters(first)[1] && now[2] > counters(first)[2] ? scan : undefined;
  }, 'items and bytes to increase across multiple polls', 120_000, 1000);
}

export async function pausedProgressStops(id: string, paused: any): Promise<void> {
  for (let sample = 0; sample < 3; sample++) {
    await new Promise(resolve => setTimeout(resolve, 1000));
    const scan = await job(id);
    assert.equal(scan?.state, 'paused');
    assert.deepEqual(counters(scan), counters(paused), 'paused progress must stay fixed');
  }
}

/** Cleanup always cancels; unexpected completion is never treated as successful cancellation. */
export async function cancelScan(id: string): Promise<void> {
  const current = await job(id);
  assert.ok(current, 'owned test scan must still exist');
  if (current.state !== 'cancelled') {
    assert.ok(!terminalStates.includes(current.state), `scan unexpectedly ended: ${current.state}`);
    const answer = await api('job.cancel', { job_id: id });
    assert.equal(answer.exit, 0);
  }
  await assertCancelled(id);
}

/** Readbacks only: a failed UI cancel must not be rescued by an API mutation. */
export async function assertCancelled(id: string): Promise<void> {
  await poll(async () => {
    const scan = await job(id);
    if (terminalStates.includes(scan?.state)) assert.equal(scan.state, 'cancelled');
    return scan?.state === 'cancelled' || undefined;
  }, 'cancelled scan', 120_000, 500);
  await poll(async () => !(await workerRunning(id)) || undefined, 'scan worker/root process to exit', 30_000, 1000);
}
