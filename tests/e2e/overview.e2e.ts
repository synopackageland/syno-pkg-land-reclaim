// Overview: one storage-space card per scannable volume, each with its usage bar.
import assert from 'node:assert/strict';
import { test } from '@e2e-dev/web';
import { api, goTo, openReclaim, t } from './support/reclaim.ts';

test('Overview shows a space card for every scannable volume', { session: 'dsm', timeout: 120_000 }, async ({ app }) => {
  const root = await openReclaim(app);
  await goTo(root, 'overview.title');
  const volumes = (await api('volumes.list')).volumes.filter((volume: any) => volume.capabilities?.scan);
  assert.ok(volumes.length > 0, 'volumes.list lists a scannable volume');
  for (const volume of volumes) {
    const name = volume.mount.path.replace(/^\//, '');
    const card = root.locator('button.volume-card').filter({ has: root.page().locator('.volume-card-name', { hasText: new RegExp(`^${name}$`) }) });
    await card.waitFor({ timeout: 20_000 });
    assert.equal(await card.getByRole('progressbar').count(), 1, `${name} card shows its usage bar`);
  }
  await root.getByRole('button', { name: t('shell.newScan'), exact: true }).waitFor();
});

test('the agent reads the Overview space cards as a user would', { session: 'dsm', timeout: 180_000 }, async ({ app, agent }) => {
  const root = await openReclaim(app);
  await goTo(root, 'overview.title');
  await agent.assert('The front DSM window is Syno Reclaim on its 總覽 page, showing at least one storage-space card with a name such as volume1 and a usage bar.');
});
