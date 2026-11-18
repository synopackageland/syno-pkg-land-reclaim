// Recycle Bin: the page lists the share recycle bins of the volume report and
// opens the scratch share's detail beside the list, with its oldest items
// (since eee79b31 the detail's own heading, not "Oldest items in <share>").
import assert from 'node:assert/strict';
import { test } from '@e2e-dev/web';
import { api, goTo, openReclaim, t } from './support/reclaim.ts';

test('Recycle Bin lists the share and opens its oldest items', { session: 'dsm', timeout: 120_000 }, async ({ app }) => {
  const share = process.env.E2E_FIXTURE_SHARE || 'share';
  const root = await openReclaim(app);
  await goTo(root, 'recycle.title');
  const { shares } = await api('recycle.list', { target: '/volume1' });
  assert.ok(shares.some((entry: any) => entry.share.path === share), `recycle.list reports the ${share} recycle bin`);
  await root.getByRole('button', { name: t('recycle.oldest.showFor', { share }), exact: true }).click({ timeout: 20_000 });
  const detail = root.getByTestId('recycle-detail');
  await detail.getByRole('heading', { level: 2, name: share, exact: true }).waitFor({ timeout: 30_000 });
  await detail.getByRole('heading', { level: 3, name: t('recycle.detail.oldest'), exact: true }).waitFor();
  // The oldest items' list shows only once recycle.list returned them for the share.
  await detail.getByText(t('recycle.oldest.col.path'), { exact: true }).waitFor({ timeout: 30_000 });
  await root.getByRole('button', { name: t('recycle.oldest.hideFor', { share }), exact: true }).click();
  await detail.waitFor({ state: 'detached' });
});
