// New scan → Scanning progress → Explore → Cleanup review and apply → History,
// on a fixture this run creates under the scratch share and removes afterwards.
import assert from 'node:assert/strict';
import { afterAll, beforeAll, describe } from 'e2e';
import { test } from '@e2e-dev/web';
import { api, createFixture, fixtureEntries, fixtureFor, goTo, job as jobNow, openReclaim, poll, removeFixture, startShareScan, t, terminalStates } from './support/reclaim.ts';

const fixture = fixtureFor('cleanup');
let scanJob: string | undefined;

describe('scan and clean up an owned fixture', { serial: true, session: 'dsm' }, () => {
  beforeAll(() => createFixture(fixture));
  afterAll(() => removeFixture(fixture));

  test('New scan scans only the chosen shared folder and shows its progress', { timeout: 20 * 60_000 }, async ({ app }) => {
    const root = await openReclaim(app);
    const job = await startShareScan(root, fixture.share);
    scanJob = job.job_id;
    // The Scanning page while it runs; a short scan may already be done.
    if (!terminalStates.includes(job.state)) {
      await root.getByRole('list', { name: t('scan.phases.title') }).waitFor({ timeout: 20_000 });
      assert.ok(await root.getByRole('progressbar').first().isVisible(), 'the Scanning page shows the progress ring');
    }
    const done = await poll(async () => {
      const entry = await jobNow(scanJob!);
      return terminalStates.includes(entry?.state) ? entry : undefined;
    }, 'the scan to finish', 15 * 60_000);
    assert.equal(done.state, 'completed', `the scan completed (state ${done.state})`);
    const scan = (await api('history.list')).scans.find((entry: any) => entry.job_id === scanJob);
    assert.deepEqual(scan?.targets?.map((target: any) => target.path), [`/volume1/${fixture.share}`], 'the scan covered only the chosen share');
    // On completion the app returns to Overview.
    await root.getByRole('heading', { level: 1, name: t('overview.title'), exact: true }).waitFor({ timeout: 30_000 });
  });

  test('Explore drills into the fixture and shows its folders', { timeout: 120_000 }, async ({ app }) => {
    const root = await openReclaim(app);
    await goTo(root, 'explore.title');
    for (const path of [`/volume1/${fixture.share}`, fixture.root]) await root.getByRole('button', { name: path, exact: true }).click({ timeout: 20_000 });
    for (const path of [fixture.keep, fixture.remove]) await root.getByRole('button', { name: path, exact: true }).waitFor({ timeout: 20_000 });
    // The treemap leaves out tiles too small to draw, so it shows two or three of the three folders.
    const [before, after] = t('explore.treemap.aria').split('{count}');
    const treemap = root.getByRole('group', { name: new RegExp(`^${before}\\d+${after}$`) });
    await treemap.waitFor({ timeout: 20_000 });
    const tiles = Number((await treemap.getAttribute('aria-label'))?.slice(before.length, -after.length || undefined));
    assert.ok(tiles >= 2 && tiles <= 3, `the treemap shows the fixture's folders (${tiles} tiles)`);
  });

  test('Cleanup review deletes only the chosen folder', { timeout: 10 * 60_000 }, async ({ app }) => {
    const root = await openReclaim(app);
    const page = root.page();
    if (!await root.getByRole('button', { name: fixture.remove, exact: true }).isVisible()) {
      await goTo(root, 'explore.title');
      for (const path of [`/volume1/${fixture.share}`, fixture.root]) await root.getByRole('button', { name: path, exact: true }).click({ timeout: 20_000 });
    }
    await root.getByRole('checkbox', { name: t('explore.basket.select', { name: 'remove' }), exact: true }).check();
    await root.getByRole('button', { name: t('explore.basket.review'), exact: true }).click();
    const dialog = page.getByRole('dialog', { name: t('review.title') });
    await dialog.waitFor({ timeout: 60_000 });
    await dialog.getByRole('checkbox', { name: t('review.confirmCheck'), exact: true }).check();
    await dialog.getByRole('button', { name: t('review.confirmDelete', { count: 1 }), exact: true }).click();
    const result = page.getByRole('dialog').filter({ hasText: t('review.result.completeTitle') });
    await result.waitFor({ timeout: 5 * 60_000 });
    assert.deepEqual(await fixtureEntries(fixture.remove), [], 'the remove folder is gone from the NAS');
    assert.deepEqual(await fixtureEntries(fixture.keep), [fixture.keep, `${fixture.keep}/control.bin`], 'the keep folder is untouched');
    const jobs = (await api('jobs.list')).jobs.filter((job: any) => job.kind === 'cleanup-apply');
    assert.ok(jobs.some((job: any) => job.state === 'completed'), 'jobs.list records a completed cleanup');
    await result.getByRole('button', { name: t('common.close'), exact: true }).last().click();
    await result.waitFor({ state: 'hidden', timeout: 10_000 });
  });

  test('History lists the scan as completed', { timeout: 120_000 }, async ({ app }) => {
    const root = await openReclaim(app);
    await goTo(root, 'history.title');
    const scans = (await api('history.list')).scans;
    assert.equal(scans.find((scan: any) => scan.job_id === scanJob)?.outcome, 'completed', 'history.list records the scan');
    await root.getByRole('row').filter({ hasText: fixture.share }).filter({ hasText: t('history.outcome.completed') }).first().waitFor({ timeout: 20_000 });
  });

  // Regression: no progress bar for the first ~10 s; "執行中" and "取消清理" stay after completion.
  test('[R3] a cleanup shows its progress bar at once and no running state once done', { timeout: 15 * 60_000 }, async ({ app }) => {
    const root = await openReclaim(app);
    const page = root.page();
    await goTo(root, 'explore.title');
    for (const path of [`/volume1/${fixture.share}`, fixture.root]) await root.getByRole('button', { name: path, exact: true }).click({ timeout: 20_000 });
    await root.getByRole('checkbox', { name: t('explore.basket.select', { name: 'again' }), exact: true }).check();
    await root.getByRole('button', { name: t('explore.basket.review'), exact: true }).click();
    const dialog = page.getByRole('dialog').filter({ has: page.getByRole('checkbox', { name: t('review.confirmCheck'), exact: true }) });
    // Right after the first cleanup the plan check has taken over a minute; R3 is about what follows it.
    const planStarted = Date.now();
    await dialog.waitFor({ timeout: 5 * 60_000 });
    console.log(`[R3] the cleanup plan took ${Date.now() - planStarted} ms`);
    const before = new Set((await api('jobs.list')).jobs.map((entry: any) => entry.job_id));
    await dialog.getByRole('checkbox', { name: t('review.confirmCheck'), exact: true }).check();
    await dialog.getByRole('button', { name: t('review.confirmDelete', { count: 1 }), exact: true }).click();
    const running = page.getByRole('dialog').filter({ hasText: t('review.applying.title') }).or(page.getByRole('dialog').filter({ hasText: t('review.result.completeTitle') }));
    const shownAt = Date.now();
    await running.getByRole('progressbar').first().waitFor({ timeout: 10_000 }).catch(() => undefined);
    const barAfterMs = Date.now() - shownAt;
    const cleanup = await poll(async () => (await api('jobs.list')).jobs.find((entry: any) => !before.has(entry.job_id) && entry.kind === 'cleanup-apply'),
      'the cleanup job', 30_000, 500);
    await poll(async () => terminalStates.includes((await jobNow(cleanup.job_id))?.state) || undefined, 'the cleanup to finish', 5 * 60_000, 1000);
    assert.deepEqual(await fixtureEntries(fixture.again), [], 'the again folder is gone from the NAS');
    await page.waitForTimeout(3000);
    const shown = page.getByRole('dialog').last();
    const leftovers: string[] = [];
    for (const text of [t('review.jobState.running'), t('review.applying.cancel')]) {
      if (await shown.getByText(text, { exact: true }).count()) leftovers.push(text);
    }
    await shown.getByRole('button', { name: t('common.close'), exact: true }).last().click().catch(() => undefined);
    assert.deepEqual({ barWithin2s: barAfterMs < 2000, leftovers }, { barWithin2s: true, leftovers: [] },
      `the progress bar shows within 2 s of confirming (took ${barAfterMs} ms), and the finished cleanup shows no running state or cancel button`);
  });
});
