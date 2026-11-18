// Regression: right after a scan starts the page says no scan runs,
// and a paused scan keeps the 掃描中 title. Cleanup verifies a successful
// resume, then cancels the scan instead of waiting for the full walk.
import assert from 'node:assert/strict';
import { test } from '@e2e-dev/web';
import { api, job, openReclaim, poll, t, terminalStates } from './support/reclaim.ts';
import { cancelScan, startVolumeScan } from './support/scan-lifecycle.ts';

test('[R2] a starting scan never says none runs, and a paused scan is not titled 掃描中', { session: 'dsm', timeout: 15 * 60_000 }, async ({ app }) => {
  const root = await openReclaim(app);
  const page = root.page();
  // A tiny fixture share can finish before the six-second title observation.
  // Use the bounded volume lifecycle; teardown always stops the original job.
  const started = await startVolumeScan(root);
  try {
    let idleWhileRunning = 0;
    for (const end = Date.now() + 6000; Date.now() < end; await page.waitForTimeout(200)) {
      if (await root.getByText(t('scan.idle.title'), { exact: true }).isVisible()) idleWhileRunning++;
    }
    await root.getByRole('button', { name: t('scan.pause'), exact: true }).click({ timeout: 20_000 });
    const confirm = page.getByRole('dialog', { name: t('scan.pause.confirmTitle') });
    if (await confirm.waitFor({ timeout: 3000 }).then(() => true, () => false)) await confirm.getByRole('button', { name: t('scan.pause'), exact: true }).click();
    const paused = await poll(async () => {
      const entry = await job(started.job_id);
      return ['paused', ...terminalStates].includes(entry?.state) ? entry : undefined;
    }, 'the scan to pause', 120_000, 1000);
    assert.equal(paused.state, 'paused', `the scan paused (state ${paused.state})`);
    await page.waitForTimeout(2000);
    const title = (await root.getByRole('heading', { level: 1 }).first().innerText()).trim();
    assert.equal(idleWhileRunning, 0, `"${t('scan.idle.title')}" never shows while the scan runs`);
    assert.notEqual(title, t('scan.title'), 'the paused scan page is not titled 掃描中');
  } finally {
    try {
      const entry = await job(started.job_id);
      if (entry?.state === 'paused' || entry?.state === 'interrupted') {
        const resumed = await api('job.resume', { job_id: started.job_id });
        assert.equal(resumed.exit, 0, 'resume was accepted');
        assert.equal(resumed.job_id, started.job_id, 'resume kept the original scan');
      }
      await poll(async () => {
        const current = await job(started.job_id);
        assert.ok(!terminalStates.includes(current?.state), `resume failed: ${current?.state}`);
        return current?.state === 'running' ? current : undefined;
      }, 'the resumed scan to run', 120_000, 300);
    } finally {
      // Always cancel the original job, including when resume or its assertion fails.
      await cancelScan(started.job_id);
    }
  }
});
