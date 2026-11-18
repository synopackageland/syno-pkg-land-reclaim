import { createFixture, fixtureFor, removeFixture, rewriteBusinessFixture } from './support/reclaim.ts';
// Volume lifecycle checks are bounded by progress readbacks, never full-scan completion.
// These scenarios are intentionally opt-in NAS acceptance tests.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from '@e2e-dev/web';
import { checkpointExists, workerRunning } from './support/scan-worker.ts';
import { assertCancelled, cancelScan, growing, pausedProgressStops, startVolumeScan } from './support/scan-lifecycle.ts';
import { goTo, job, openReclaim, poll, t, terminalStates } from './support/reclaim.ts';

test('UI volume pause and Continue preserve growing progress after business writes; cancellation allows a new scan', { session: 'dsm', timeout: 10 * 60_000 }, async ({ app }) => {
  const root = await openReclaim(app);
  const page = root.page();
  const business = fixtureFor('resume-ui');
  assert.ok(!process.env.E2E_VOLUME || process.env.E2E_VOLUME === '/volume1', 'business fixture must be on the selected volume');
  try {
    await createFixture(business);
    const started = await startVolumeScan(root);
    try {
      await growing(started.job_id);
      assert.equal(await workerRunning(started.job_id), true);
      await root.locator('.progress-ring [role="progressbar"]').waitFor();
      await root.getByRole('button', { name: t('scan.pause'), exact: true }).click();
      const pause = page.getByRole('dialog', { name: t('scan.pause.confirmTitle') });
      if (await pause.isVisible()) await pause.getByRole('button', { name: t('scan.pause'), exact: true }).click();
      const paused = await poll(async () => {
        const current = await job(started.job_id);
        assert.ok(!terminalStates.includes(current?.state), `pause failed: ${current?.state}`);
        return current?.state === 'paused' ? current : undefined;
      }, 'durably paused scan', 120_000, 500);
      assert.ok(paused.progress.entries > 0 && paused.progress.items > 0);
      assert.equal(await checkpointExists(started.job_id), true);
      assert.equal(await checkpointExists(started.job_id, business.root), true, 'fixture share must be among saved units; choose an early E2E_FIXTURE_SHARE');
      await pausedProgressStops(started.job_id, paused);
      await rewriteBusinessFixture(business);
      await poll(async () => !(await workerRunning(started.job_id)) || undefined, 'paused worker to exit', 30_000, 1000);
      await root.getByRole('button', { name: t('scan.resume'), exact: true }).click();
      const resume = page.getByRole('dialog', { name: t('scan.resume.title') });
      await resume.getByRole('radio', { name: new RegExp('^' + t('scan.mode.gentle') + '(?:\\s|$)') }).check();
      await resume.getByRole('button', { name: t('scan.resume'), exact: true }).click();
      await resume.waitFor({ state: 'hidden' });
      const running = await poll(async () => {
        const current = await job(started.job_id);
        assert.equal(current?.job_id, paused.job_id);
        assert.ok(!terminalStates.includes(current?.state), `resume failed: ${current?.state}`);
        assert.ok((current.progress?.entries ?? 0) >= paused.progress.entries, 'entry count never resets');
        assert.ok((current.progress?.items ?? 0) >= paused.progress.items, 'item count never resets');
        return current.state === 'running' && current.phase === 'scan' && current.profile === 'low' ? current : undefined;
      }, 'original scan to run in Gentle mode', 60_000, 300);
      assert.ok(running.resumes > paused.resumes);
      assert.equal(await workerRunning(started.job_id), true, 'resume started a worker');
      await growing(started.job_id, paused);
      await root.getByRole('button', { name: t('scan.cancel'), exact: true }).click();
      await page.getByRole('dialog', { name: t('scan.cancel.title') }).getByRole('button', { name: t('scan.cancel.confirm'), exact: true }).click();
      await assertCancelled(started.job_id);
      const replacement = await startVolumeScan(root);
      try {
        assert.notEqual(replacement.job_id, started.job_id);
        await growing(replacement.job_id);
      } finally {
        await cancelScan(replacement.job_id);
      }
    } finally {
      await cancelScan(started.job_id);
    }
  } finally {
    await removeFixture(business);
  }
});

test('a refused Continue shows the backend cause in the dialog and opens a new scan', { session: 'dsm', timeout: 120_000 }, async ({ app }) => {
  const root = await openReclaim(app);
  const page = root.page();
  const cause = 'Saved subvolume identity changed; start a fresh scan';
  const paused = JSON.parse(readPausedFixture());
  let freshRequest: unknown;
  // Mock recovery too: this failure scenario never mutates NAS scan data.
  await page.route('**/cgi/api.cgi', async route => {
    const request = route.request().postDataJSON();
    if (request.method === 'jobs.list') return route.fulfill({ json: { success: true, data: paused } });
    if (request.method === 'job.resume' && request.params?.fresh === true) {
      freshRequest = request.params;
      return route.fulfill({ json: { success: true, data: { command: 'detach', job_id: 'fresh-same-settings', exit: 0 } } });
    }
    if (request.method === 'job.resume') return route.fulfill({ json: { success: false, error: { code: 'checkpoint_unusable', message: cause } } });
    return route.continue();
  });
  try {
    await goTo(root, 'history.title');
    await root.getByRole('button', { name: t('history.action.resume'), exact: true }).click();
    await root.getByRole('button', { name: t('scan.resume'), exact: true }).click();
    const dialog = page.getByRole('dialog', { name: t('scan.resume.title') });
    await dialog.getByRole('button', { name: t('scan.resume'), exact: true }).click();
    await dialog.getByText(cause, { exact: false }).waitFor();
    await dialog.getByRole('button', { name: t('scan.idle.action'), exact: true }).click();
    await dialog.waitFor({ state: 'hidden' });
    assert.deepEqual(freshRequest, { job_id: paused.jobs[0].job_id, fresh: true });
    assert.equal(await page.getByRole('dialog', { name: t('newScan.title'), exact: true }).isVisible(), false);
  } finally {
    await page.unroute('**/cgi/api.cgi');
  }
});

function readPausedFixture(): string {
  const fixture = JSON.parse(readFileSync(new URL('../../ui/fixtures/jobs.list/paused.json', import.meta.url), 'utf8'));
  fixture.jobs = fixture.jobs.filter((entry: { state: string }) => entry.state === 'paused');
  fixture.jobs[0].created = fixture.jobs[0].updated = new Date().toISOString();
  return JSON.stringify(fixture);
}

test('UI Fast to Gentle to Fast applies to the same running job; Cancel ends it and its worker', { session: 'dsm', timeout: 10 * 60_000 }, async ({ app }) => {
  const root = await openReclaim(app);
  const page = root.page();
  const started = await startVolumeScan(root);
  try {
    for (const [mode, profile] of [['gentle', 'low'], ['fast', 'fast']] as const) {
      await root.getByRole('button', { name: t(mode === 'gentle' ? 'scan.changeMode.toGentle' : 'scan.changeMode.toFast'), exact: true }).click();
      const changed = await poll(async () => {
        const current = await job(started.job_id);
        assert.equal(current?.state, 'running', 'mode changes keep the original scan running');
        return current.profile === profile ? current : undefined;
      }, `running scan to apply ${mode}`, 60_000, 500);
      assert.equal(changed.job_id, started.job_id);
      assert.equal(changed.resumes, 0, 'mode switching never restarts the worker');
      await root.locator('.stat-card').filter({ hasText: t('scan.stats.mode') }).getByText(t(`scan.mode.${mode}`), { exact: true }).waitFor();
    }
    assert.equal(await workerRunning(started.job_id), true);
    await root.getByRole('button', { name: t('scan.cancel'), exact: true }).click();
    await page.getByRole('dialog', { name: t('scan.cancel.title') }).getByRole('button', { name: t('scan.cancel.confirm'), exact: true }).click();
    const cancelled = await poll(async () => {
      const current = await job(started.job_id);
      return terminalStates.includes(current?.state) ? current : undefined;
    }, 'cancelled job', 120_000, 500);
    assert.equal(cancelled.state, 'cancelled');
    await poll(async () => !(await workerRunning(started.job_id)) || undefined, 'cancelled worker to exit', 30_000, 1000);
  } finally {
    await cancelScan(started.job_id);
  }
});
