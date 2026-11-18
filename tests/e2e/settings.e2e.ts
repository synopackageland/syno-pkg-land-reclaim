// Settings at the bottom of the sidebar, and the approval state it shows.
import assert from 'node:assert/strict';
import { test } from '@e2e-dev/web';
import { appNavigation, api, goTo, openReclaim, t } from './support/reclaim.ts';

const SYSTEM_PARTITION_WARNING_KEY = 'synoreclaim.settings.systemPartitionWarning';

// Since 1569 (8819f051) Settings has no cleanup switch and no version: the low-space warning is
// the switch it keeps, stored in this browser, and the support bundle needs the selected volume.
test('Settings is the last sidebar item and shows the stored warning switch and the support bundle', { session: 'dsm', timeout: 120_000 }, async ({ app }) => {
  const root = await openReclaim(app);
  const items = appNavigation(root.page()).getByRole('button');
  assert.equal((await items.last().innerText()).trim(), t('settings.title'), 'Settings is the last sidebar item');
  await goTo(root, 'settings.title');
  const warn = root.getByRole('switch', { name: t('settings.systemPartition.warn'), exact: true });
  await warn.waitFor();
  const stored = await root.page().evaluate(key => localStorage.getItem(key), SYSTEM_PARTITION_WARNING_KEY);
  assert.equal(await warn.isChecked(), stored !== 'false', 'the switch shows the stored low-space warning choice');
  const download = root.getByRole('button', { name: t('settings.diagnostics.download'), exact: true });
  await download.waitFor();
  assert.ok(await download.isEnabled(), 'the support bundle can be downloaded for the selected volume');
});

test('the approved app shows its executor status and asks for no password', { session: 'dsm', timeout: 120_000 }, async ({ app }) => {
  const root = await openReclaim(app);
  assert.equal((await api('authorization.status')).state, 'ready', 'authorization.status is ready');
  await goTo(root, 'settings.title');
  await root.locator('[data-executor-status]').first().waitFor({ timeout: 20_000 });
  assert.equal(await root.locator('#delegate-password').count(), 0, 'no password prompt while approved');
});

// SDK 0.10.5: the SDK sections at the top of Settings (executor, portal) keep their size
// while they load, so what follows them moves at most 2 px. PerformanceObserver misses layout
// shifts in some environments, so the page samples the positions itself on every frame. Their
// loading rows are [data-placeholder-row] now, not [data-ui-skeleton-line].
test('[CLS] Settings content below the SDK sections moves at most 2 px while it loads', { session: 'dsm', timeout: 120_000 }, async ({ app }) => {
  const root = await openReclaim(app);
  const page = root.page();
  // The first open can take ~10 s to show Overview; the 5 s window measures Settings, not that.
  await page.getByRole('heading', { level: 1, name: t('overview.title'), exact: true }).waitFor({ timeout: 30_000 });
  // The low-space warning and the support bundle download are the rows below the SDK sections.
  const sampling = page.evaluate(async ([warnLabel, downloadLabel]) => {
    const tops: { at: number; warn: number; download: number }[] = [];
    let skeletons = 0, placeholders = 0;
    const start = performance.now();
    const leaf = (text: string) => [...document.querySelectorAll('main *')].find(element => element.childElementCount === 0 && element.textContent?.trim() === text);
    while (performance.now() - start < 5000) {
      await new Promise(next => requestAnimationFrame(next));
      skeletons = Math.max(skeletons, document.querySelectorAll('main [data-ui-skeleton-line]').length);
      placeholders = Math.max(placeholders, document.querySelectorAll('main [data-placeholder-row]').length);
      const warn = leaf(warnLabel), download = leaf(downloadLabel);
      if (warn && download) tops.push({ at: Math.round(performance.now() - start), warn: warn.getBoundingClientRect().top, download: download.getBoundingClientRect().top });
    }
    return { tops, skeletons, placeholders };
  }, [t('settings.systemPartition.warn'), t('settings.diagnostics.download')]);
  await goTo(root, 'settings.title');
  const { tops, skeletons, placeholders } = await sampling;
  console.log(`[CLS] loading rows: ${placeholders} [data-placeholder-row], ${skeletons} [data-ui-skeleton-line]`);
  assert.ok(tops.length > 0, 'Settings showed its rows within 5 s');
  const shift = (key: 'warn' | 'download') => Math.max(...tops.map(top => top[key])) - Math.min(...tops.map(top => top[key]));
  assert.deepEqual({ skeletons, warnWithin2px: shift('warn') <= 2, downloadWithin2px: shift('download') <= 2 },
    { skeletons: 0, warnWithin2px: true, downloadWithin2px: true },
    `the SDK sections load with [data-placeholder-row], and rows below them move at most 2 px (low-space warning ${shift('warn')} px, support bundle ${shift('download')} px; first seen at ${tops[0].at} ms)`);
});
