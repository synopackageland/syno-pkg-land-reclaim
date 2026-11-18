import assert from 'node:assert/strict';
import { afterAll, beforeAll, describe } from 'e2e';
import { test } from '@e2e-dev/web';
import { api, goTo, loadStrings, openReclaim, poll, t } from './support/reclaim.ts';
import { cancelCtlScan, ctl, waitForTinyShareScan } from './support/ctl.ts';
import { createFeatureFixture, featureExists, featureFixture, removeFeatureFixture, required,
  type FeatureFixture } from './support/feature-fixture.ts';

// Each test runs independently so a missing package/snapshot report cannot skip recycle/history.
describe('main UI feature outcomes on an owned share', { session: 'dsm' }, () => {
  let fixture: FeatureFixture;
  let scanId: string | undefined;
  let fixtureScan: Promise<void> | undefined;
  beforeAll(async () => {
    assert.equal(required('RECLAIM_REAL_NAS'), '1');
    fixture = featureFixture();
    await createFeatureFixture(fixture);
  });
  function ensureFixtureReport(): Promise<void> {
    // Keep the same promise after a refusal too: another test must never resubmit
    // an uncertain start. Packages/rootfs remain independent of scan acceptance.
    return fixtureScan ??= (async () => {
      const accepted = await ctl('scans.start', { targets: [fixture.sharePath], mode: 'fast', snapshots: true });
      assert.equal(accepted.accepted, true, 'An unrelated active scan cannot stand in for this fixture');
      scanId = accepted.job_id;
      assert.ok(scanId);
      await waitForTinyShareScan(scanId, fixture.sharePath);
    })();
  }
  afterAll(async () => {
    // Never remove fixture data while an owned scan might still read it.
    if (scanId) await cancelCtlScan(scanId);
    if (fixture) await removeFeatureFixture(fixture);
  });

  test('Packages loads byte sizes and opens an existing package detail and path', { timeout: 10 * 60_000 }, async ({ app }) => {
    const root = await openReclaim(app);
    await goTo(root, 'packages.title');
    const doc = await api('top.list', { target: fixture.volume, kind: 'all', by: 'reclaimable', limit: 200 });
    const lines = doc.results[0]?.services.lines ?? [];
    const line = lines.find((line: any) => line.service === 'container_manager' && line.metrics.physical > 0)
      ?? lines.find((line: any) => !['recycle_bin', 'folder_metadata'].includes(line.service) && line.metrics.physical > 0);
    assert.ok(line, 'The report contains an actual package/service row');
    const name = t(`code.service.${line.service}`) === `code.service.${line.service}` ? line.package : t(`code.service.${line.service}`);
    const toggleName = t('packages.detail.show', { name });
    const toggle = root.getByRole('button', { name: toggleName, exact: true }).first();
    const row = root.getByRole('row').filter({ has: root.page().getByRole('button', { name: toggleName, exact: true }) });
    await row.waitFor({ timeout: 180_000 });
    assert.match(await row.innerText(), /\d/, 'The package has a measured size');
    await toggle.click();
    const detail = root.locator('[data-ui-list-detail-pane]');
    await detail.getByRole('heading', { name: t('packages.detail.how'), exact: true }).waitFor();
    assert.ok((await detail.innerText()).length > name.length, 'The selected package has management advice');
    await detail.locator('summary').click();
    const path = detail.locator('button.path-button').first();
    if (await path.count()) {
      const destination = await path.getAttribute('title');
      await path.click();
      await root.getByRole('heading', { level: 1, name: t('explore.title'), exact: true }).waitFor();
      assert.ok(destination);
      const breadcrumb = root.getByLabel(t('explore.breadcrumb.label'), { exact: true });
      await breadcrumb.waitFor({ timeout: 180_000 });
      assert.ok((await breadcrumb.innerText()).includes(destination.split('/').at(-1)!), 'Explore opens the selected package path');
    }
  });

  test('Recycle Bin deletes only the selected test-created item through cleanup review', { timeout: 10 * 60_000 }, async ({ app }) => {
    await ensureFixtureReport();
    const root = await openReclaim(app);
    const page = root.page();
    await goTo(root, 'recycle.title');
    const answer = await api('recycle.list', { target: fixture.sharePath });
    const key = answer.shares.find((s: any) => s.share.path === fixture.share || s.share.path === fixture.sharePath)?.share.path;
    assert.ok(key, 'The completed fixture share has a recycle report');
    await root.getByRole('button', { name: t('recycle.oldest.showFor', { share: key }), exact: true }).click();
    const row = root.getByRole('row').filter({ hasText: fixture.name });
    await row.getByRole('checkbox').check({ timeout: 30_000 });
    // Recycle adds to the shared basket; its review action lives in Explore.
    await goTo(root, 'explore.title');
    await root.getByRole('button', { name: t('explore.basket.review'), exact: true }).click();
    const review = page.getByRole('dialog', { name: t('review.title'), exact: true });
    await review.waitFor({ timeout: 5 * 60_000 });
    // Selecting exactly our row must produce exactly one review entry.
    await review.getByRole('button', { name: t('review.confirmDelete', { count: 1 }), exact: true }).waitFor();
    await review.getByRole('checkbox', { name: t('review.confirmCheck'), exact: true }).check();
    await review.getByRole('button', { name: t('review.confirmDelete', { count: 1 }), exact: true }).click();
    const result = page.getByRole('dialog').filter({ hasText: t('review.result.completeTitle') });
    await result.waitFor({ timeout: 5 * 60_000 });
    assert.equal(await featureExists(fixture.recycled, fixture), false, 'The selected recycled item is absent on disk');
    assert.equal(await featureExists(`${fixture.root}/control.bin`, fixture), true, 'The unselected live control file remains');
    await result.getByRole('button', { name: t('common.close'), exact: true }).last().click();
  });

  test('History opens the selected saved scan result with its own target and measured totals', { timeout: 10 * 60_000 }, async ({ app }) => {
    await ensureFixtureReport();
    assert.ok(scanId);
    const root = await openReclaim(app);
    await goTo(root, 'history.title');
    const record = (await api('history.list')).scans.find((s: any) => s.job_id === scanId);
    assert.equal(record?.outcome, 'completed');
    const row = root.getByRole('row').filter({ hasText: fixture.share }).first();
    await row.getByRole('button', { name: t('history.action.view'), exact: true }).click();
    const shown = root.page().getByTestId('history-result');
    await shown.waitFor({ timeout: 180_000 });
    assert.equal(await shown.getAttribute('data-report-job-id'), scanId);
    const result = await api('job.result', { job_id: scanId });
    assert.equal(result.job_id, scanId);
    assert.ok((await shown.innerText()).includes(fixture.sharePath), 'The selected saved result shows its own scanned target');
    assert.ok(result.filesystems.some((fs: any) => fs.totals?.files >= 2 && fs.totals?.physical >= 3 * 1048576));
    assert.equal(await featureExists(`${fixture.root}/control.bin`, fixture), true);
    await root.page().getByRole('dialog').getByRole('button', { name: t('common.close'), exact: true }).last().click();
  });

  test('System cleanup creates a plan and shows its items at confirmation without applying', { timeout: 20 * 60_000 }, async ({ app }) => {
    const root = await openReclaim(app);
    const page = root.page();
    const initialJobs = (await api('jobs.list')).jobs;
    const before = new Set(initialJobs.filter((j: any) => j.kind === 'rootfs-apply').map((j: any) => j.job_id));
    const knownPlans = new Set(initialJobs.filter((j: any) => j.kind === 'rootfs-plan').map((j: any) => j.job_id));
    const applyCalls: string[] = [];
    const guard = async (route: import('playwright-core').Route) => {
      let body;
      try { body = route.request().postDataJSON(); } catch { return route.continue(); }
      if (['rootfs.apply', 'ctl.rootfs.apply'].includes(body?.method)) {
        applyCalls.push(body.method);
        return route.abort();
      }
      return route.continue();
    };
    await page.route('**/cgi/api.cgi', guard);
    try {
      await goTo(root, 'overview.title');
      assert.notEqual((await api('volumes.list')).system_partition?.level, 'ok', 'Fixture NAS must offer system cleanup; never fill rootfs for a test');
      await root.locator('button.volume-card').filter({ hasText: t('rootfs.name') }).click();
      await root.locator('.rootfs-movable-button').click({ timeout: 90_000 });
      const dialog = page.getByRole('dialog', { name: t('rootfsClean.title'), exact: true });
      await dialog.waitFor();
      const destinations = dialog.getByTestId('rootfs-destination');
      const search = destinations.getByRole('searchbox');
      if (await search.count()) {
        await search.fill(fixture.share);
        const collapsed = destinations.locator('button[aria-expanded="false"]');
        for (const group of await collapsed.all()) await group.click();
      }
      await destinations.getByRole('radio', { name: new RegExp(fixture.share) }).check();
      await dialog.getByRole('button', { name: t('rootfsClean.action.next'), exact: true }).click();
      await dialog.getByRole('checkbox', { name: t('rootfsClean.review.check'), exact: true }).waitFor({ timeout: 5 * 60_000 });
      const confirm = dialog.getByRole('button', { name: t('rootfsClean.action.yes'), exact: true });
      assert.equal(await confirm.isDisabled(), true, 'Confirmation remains unchecked');
      const planned = await poll(async () => {
        const jobs = (await api('jobs.list')).jobs;
        return jobs.find((j: any) => j.kind === 'rootfs-plan' && !knownPlans.has(j.job_id) && j.state === 'completed');
      }, 'the saved rootfs plan', 10 * 60_000);
      const plan = await api('job.result', { job_id: planned.job_id });
      assert.ok(plan.plan_id, 'A real sealed plan exists');
      assert.ok(plan.items.length || plan.tools.length, 'The confirmation has real planned items or tools');
      for (const item of plan.items) {
        await dialog.getByText(t(`rootfsClean.category.${item.category}`), { exact: false }).first().waitFor();
      }
      // Close the dialog without checking consent or clicking the apply button.
      await dialog.getByRole('button', { name: t('common.close'), exact: true }).first().click();
    } finally {
      await page.unroute('**/cgi/api.cgi', guard);
      assert.deepEqual(applyCalls, [], 'The browser never submitted rootfs.apply');
      const after = (await api('jobs.list')).jobs.filter((j: any) => j.kind === 'rootfs-apply').map((j: any) => j.job_id);
      assert.ok(after.every((id: string) => before.has(id)), 'No system apply job was created');
    }
  });
});

// Inventory differs by NAS. Only explicitly configured disposable objects are read.
// An owned shared folder is required; unavailable LUN/VM fixtures are reported as skips.
for (const kind of ['share', 'lun', 'vm'] as const) {
  test(`Snapshots selects a ${kind} target and verifies time, state, exclusive bytes and selection`, { session: 'dsm', timeout: 10 * 60_000, skip: kind !== 'share' && !process.env[`E2E_SNAPSHOT_${kind.toUpperCase()}`] ? `No disposable ${kind} snapshot fixture configured; existing NAS objects are excluded` : false }, async ({ app }) => {
    const target = required(`E2E_SNAPSHOT_${kind.toUpperCase()}`);
    const root = await openReclaim(app);
    const page = root.page();
    await goTo(root, 'snapshots.title');
    const picker = root.getByRole('combobox', { name: t('snapshots.share.label'), exact: true });
    if (await picker.isEnabled()) {
      await picker.click();
      await page.getByRole('option').filter({ hasText: target }).click();
    }
    assert.ok((await picker.innerText()).includes(target), 'The requested target is selected');
    const targetKind = root.getByTestId('target-kind');
    await targetKind.waitFor();
    assert.ok((await targetKind.innerText()).includes(t(`snapshots.target.type.${kind === 'vm' ? 'vmm' : kind}`)), 'Picker resolves the requested target kind');
    const snapshots = root.locator('.snapshot-table');
    const keys = await snapshots.locator('[data-snapshot-key]').evaluateAll(nodes => nodes.map(n => n.getAttribute('data-snapshot-key')));
    assert.ok(keys.length > 1, 'Snapshot fixture has multiple rows');
    const volume = featureFixture().volume;
    const document = await api('snapshots.list', { target: kind === 'share' ? `${volume}/${target}` : volume });
    const share = document.results.flatMap((r: any) => r.shares).find((s: any) =>
      s.snapshots.some((snapshot: any) => keys.includes(String(snapshot.subvolume_id))));
    assert.ok(share, 'Displayed snapshots belong to a real saved target report');
    assert.equal(keys.length, share.snapshots.length, 'Every reported snapshot has a row');
    assert.ok(share.snapshots.some((s: any) => s.materialized && s.exclusive_bytes > 0), 'Fixture includes measured exclusive space');
    const language = await loadStrings(page);
    if (kind === 'share') await root.getByTestId('dsm-snapshot-loading').waitFor({ state: 'hidden', timeout: 180_000 });
    for (const snapshot of share.snapshots) {
      const row = snapshots.getByRole('row').filter({ has: page.locator(`[data-snapshot-key="${snapshot.subvolume_id}"]`) });
      assert.ok(snapshot.otime, 'Fixture has a real creation timestamp');
      const date = await page.evaluate(({ date, language }) =>
        new Intl.DateTimeFormat(language, { dateStyle: 'medium', timeStyle: 'short' }).format(new Date(date)),
      { date: snapshot.otime, language });
      assert.ok((await row.innerText()).includes(date), 'The row shows the reported creation time');
      if (kind === 'share') {
        assert.equal(await row.getByLabel(t('snapshots.table.lockUnknown'), { exact: true }).count(), 0, 'DSM resolves the owned snapshot lock state');
        assert.equal(await row.getByRole('img', { name: t('snapshots.table.locked'), exact: true }).count(), 0, 'The owned ordinary snapshot is unlocked');
        assert.equal(await row.getByRole('img', { name: t('snapshots.table.immutable'), exact: true }).count(), 0, 'The owned ordinary snapshot has no retention lock');
      }
      if (snapshot.materialized && snapshot.exclusive_bytes > 0) {
        assert.match(await row.getByRole('cell').last().innerText(), /\d/, 'The row shows measured exclusive bytes');
      }
    }
    for (const key of ['time', 'lock', 'exclusive']) {
      // The resizer's accessible name is also part of a column header's name.
      await snapshots.getByRole('columnheader').filter({
        has: page.getByText(t(`snapshots.table.${key}`), { exact: true }),
      }).waitFor();
    }
    const boxes = snapshots.getByRole('row').getByRole('checkbox');
    await root.getByRole('button', { name: t('snapshots.chart.selectAll'), exact: true }).click();
    assert.ok((await boxes.count()) > 0);
    for (const box of await boxes.all()) {
      if (await box.isEnabled()) assert.equal(await box.isChecked(), true);
    }
    await snapshots.getByRole('button', { name: t('snapshots.table.clear'), exact: true }).click();
    for (const box of await boxes.all()) assert.equal(await box.isChecked(), false);
    // Fresh ordinary snapshots share today's date. Older-than is a multi-day
    // control; its date predicate is tested locally, without changing the NAS clock.
    const older = snapshots.getByRole('combobox', { name: t('snapshots.table.olderPlaceholder'), exact: true });
    if (await older.count()) {
      await older.click();
      await page.getByRole('option').first().click();
      const checked = await Promise.all((await boxes.all()).map(box => box.isChecked()));
      assert.ok(checked.some(Boolean) && checked.some(value => !value));
      await snapshots.getByRole('button', { name: t('snapshots.table.clear'), exact: true }).click();
    }
    const simulationReply = page.waitForResponse(response => {
      try { const body = response.request().postDataJSON(); return body?.method === 'snapshots.list' && Boolean(body.params?.simulate); }
      catch { return false; }
    }, { timeout: 180_000 });
    await snapshots.getByRole('row').filter({ has: page.locator(`[data-snapshot-key="${keys[0]}"]`) }).getByRole('checkbox').check();
    const response = await (await simulationReply).json();
    const simulation = response.data.results.flatMap((r: any) => r.shares).find((s: any) => s.simulation)?.simulation;
    assert.deepEqual(simulation?.selected_ids, [Number(keys[0])], 'The real simulation uses the selected snapshot ID');
    await root.getByRole('button', { name: t('snapshots.dsm.button', { place: t('code.manage_in.snapshot_replication') }), exact: true }).click();
    await page.getByRole('application', { name: t('code.manage_in.snapshot_replication'), exact: true }).waitFor({ timeout: 180_000 });
  });
}
