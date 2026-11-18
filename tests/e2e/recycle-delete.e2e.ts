// Cleanup moves shared-folder items into that folder's Recycle Bin: Explore's
// select-and-move, Duplicates' remove copies, the guidance for a shared folder
// without a Recycle Bin, the Recycle Bin page after a move, and the system
// partition cleanup that still deletes for good.
//
// It runs on the duplicates test data, reclaimdup-* shared folders on the
// authorized test NAS, and puts back what it moves with File Station's own
// Restore. Set:
//   E2E_RECYCLE_SHARE      a reclaimdup-* shared folder whose Recycle Bin is on
//   E2E_NO_RECYCLE_SHARE   a reclaimdup-* shared folder whose Recycle Bin is off
//   E2E_EXPLORE_ITEMS      top-level items of E2E_RECYCLE_SHARE to move from Explore
//   E2E_BLOCKED_ITEM       a top-level item of E2E_NO_RECYCLE_SHARE to try
//   E2E_LONG_ITEMS         folders of E2E_RECYCLE_SHARE, in the order to select them, whose
//                          move takes long enough to reload the page part-way (no symlinks
//                          or empty folders: DSM's Recycle Bin keeps neither)
//   E2E_SHOT_DIR           where the screenshots go (default .e2e/recycle-delete)
import assert from 'node:assert/strict';
import { afterAll, beforeAll, describe } from 'e2e';
import { test } from '@e2e-dev/web';
import { api, goTo, job as jobNow, openReclaim, poll, startShareScan, t, terminalStates } from './support/reclaim.ts';
import { datasetItems, datasetShare, files, recycleBin, restoreFromRecycleBin, screenshot, sharePath, tree,
  type Entry } from './support/recycle-dataset.ts';

const MOVE_TIMEOUT = 10 * 60_000;

type Root = Awaited<ReturnType<typeof openReclaim>>;
type Page = ReturnType<Root['page']>;

/** Explore at the top of a shared folder. */
async function exploreShare(root: Root, share: string): Promise<void> {
  await goTo(root, 'explore.title');
  // Explore reopens where it was left: start from the volume.
  const breadcrumb = root.getByLabel(t('explore.breadcrumb.label'), { exact: true });
  const volume = breadcrumb.getByRole('button', { name: 'volume1', exact: true });
  await volume.waitFor({ timeout: 60_000 }).then(() => volume.click(), () => undefined);
  await root.getByRole('button', { name: `/volume1/${share}`, exact: true }).click({ timeout: 60_000 });
  await root.getByRole('button', { name: t('explore.basket.review'), exact: true }).or(
    root.getByRole('checkbox').first()).first().waitFor({ timeout: 60_000 });
}

/** Empties the shared cleanup list through its own Clear button, when it shows. */
async function clearList(root: Root): Promise<void> {
  const clear = root.getByRole('button', { name: t('explore.basket.clear'), exact: true });
  if (await clear.count()) await clear.first().click();
}

async function reviewDialog(page: Page) {
  const dialog = page.getByRole('dialog', { name: t('review.title') });
  await dialog.waitFor({ timeout: 5 * 60_000 });
  return dialog;
}

/** Ticks the consent box, moves, and waits for the result; returns the result dialog. */
async function confirmMove(page: Page, dialog: ReturnType<Page['getByRole']>, count: number) {
  await dialog.getByRole('checkbox', { name: t('review.confirmCheck'), exact: true }).check();
  await dialog.getByRole('button', { name: t('review.confirmMove', { count }), exact: true }).click();
  const result = page.getByRole('dialog').filter({ hasText: t('review.result.completeTitle') });
  await result.waitFor({ timeout: MOVE_TIMEOUT });
  return result;
}

/** The same files, at the same places, of the same sizes. */
function sameFiles(after: Entry[], before: Entry[], what: string): void {
  assert.deepEqual(files(after), files(before), what);
}

describe('cleanup moves shared-folder items into the Recycle Bin', { serial: true, session: 'dsm' }, () => {
  const share = datasetShare('E2E_RECYCLE_SHARE');
  const offShare = datasetShare('E2E_NO_RECYCLE_SHARE');
  const exploreItems = datasetItems('E2E_EXPLORE_ITEMS');
  const blocked = datasetItems('E2E_BLOCKED_ITEM')[0]!;
  const longItems = datasetItems('E2E_LONG_ITEMS');
  const before = new Map<string, Entry[]>();
  // What a test moved and has not put back yet; afterAll restores it if a step failed.
  const outstanding = new Set<string>();
  let restorePage: Page | undefined;

  beforeAll(async () => {
    assert.deepEqual(await recycleBin(share), { enabled: true, adminOnly: false }, `${share} has a Recycle Bin for everyone`);
    assert.equal((await recycleBin(offShare)).enabled, false, `${offShare} has no Recycle Bin`);
    for (const item of [...exploreItems]) {
      const entries = await tree(sharePath(share, item));
      assert.ok(entries.length, `${item} exists in ${share}`);
      assert.deepEqual(await tree(sharePath(share, item, true)), [], `${item} is not in the Recycle Bin of ${share} yet`);
      before.set(item, entries);
    }
    assert.ok((await tree(sharePath(offShare, blocked))).length, `${blocked} exists in ${offShare}`);
  });

  afterAll(async () => {
    // A failed step must not leave the test data in a Recycle Bin. A move
    // runs on without the page: wait for it to end before putting anything back.
    if (!outstanding.size || !restorePage) return;
    await poll(async () => {
      const jobs = (await answer('jobs.list')).jobs;
      return jobs.some((job: any) => job.kind === 'cleanup-apply' && !terminalStates.includes(job.state)) ? undefined : true;
    }, 'running moves to end', 30 * 60_000, 5000);
    const moved: string[] = [];
    for (const item of outstanding) if ((await tree(sharePath(share, item, true))).length && !(await tree(sharePath(share, item))).length) moved.push(item);
    await restoreFromRecycleBin(restorePage, share, moved);
  });

  /** An app call that waits out DSM handing a freshly loaded page its token. */
  async function answer(method: string, params: Record<string, unknown> = {}) {
    for (const end = Date.now() + 120_000; ; await new Promise(next => setTimeout(next, 2000))) {
      try { return await api(method, params); } catch (error) {
        if (!String(error).includes('credential_not_ready') || Date.now() > end) throw error;
      }
    }
  }

  // A restored folder is a new folder: DSM's Recycle Bin recreates folders, so
  // a scan before this run may name other inodes. Plan against a fresh one.
  test('a fresh scan of the shared folder to plan against', { timeout: 30 * 60_000 }, async ({ app }) => {
    const root = await openReclaim(app);
    const started = await startShareScan(root, share);
    const done = await poll(async () => {
      const entry = await jobNow(started.job_id);
      return terminalStates.includes(entry?.state) ? entry : undefined;
    }, 'the scan to finish', 25 * 60_000, 5000);
    assert.equal(done.state, 'completed', `the scan of ${share} completed`);
    // A finished scan takes the app back to Overview: let it, before the next test navigates.
    await root.getByRole('heading', { level: 1, name: t('overview.title'), exact: true }).waitFor({ timeout: 120_000 });
    await root.page().waitForTimeout(3000);
  });

  test('Explore moves the selected items into the Recycle Bin of their shared folder', { timeout: 20 * 60_000 }, async ({ app }) => {
    const root = await openReclaim(app);
    const page = root.page();
    restorePage = page;
    await exploreShare(root, share);
    await clearList(root);
    for (const item of exploreItems) await root.getByRole('checkbox', { name: t('explore.basket.select', { name: item }), exact: true }).check();
    await root.getByRole('button', { name: t('explore.basket.review'), exact: true }).click();
    const dialog = await reviewDialog(page);
    await dialog.locator('[data-test="review-recycle"]').waitFor({ timeout: 5 * 60_000 });
    const group = dialog.locator('[data-test="review-share"]');
    assert.ok((await group.innerText()).includes(t('review.share.title', { share })), 'the plan groups the items under their shared folder');
    assert.ok((await dialog.locator('[data-test="review-everyone"]').innerText()).includes(t('review.share.everyone', { share })),
      'a Recycle Bin open to everyone is pointed out');
    for (const item of exploreItems) assert.ok((await group.innerText()).includes(sharePath(share, item)), `${item} is listed`);
    await screenshot(page, 'explore-review');

    const result = await confirmMove(page, dialog, exploreItems.length);
    for (const item of exploreItems) outstanding.add(item);
    await result.locator('[data-test="review-moved"]').waitFor();
    assert.ok((await result.locator('[data-test="review-bins"]').innerText()).includes(share), 'the result names the Recycle Bin');
    await screenshot(page, 'explore-moved');

    for (const item of exploreItems) {
      assert.deepEqual(await tree(sharePath(share, item)), [], `${item} left its place`);
      sameFiles(await poll(async () => {
        const recycled = await tree(sharePath(share, item, true));
        return files(recycled).length === files(before.get(item)!).length ? recycled : undefined;
      }, `${item} in the Recycle Bin`, 2 * 60_000, 2000), before.get(item)!, `${item} is whole in #recycle`);
    }
    const jobs = (await api('jobs.list')).jobs.filter((job: any) => job.kind === 'cleanup-apply');
    assert.ok(jobs.some((job: any) => job.state === 'completed'), 'jobs.list records the completed move');
    await result.getByRole('button', { name: t('common.close'), exact: true }).last().click();
  });

  test('the Recycle Bin page shows what Syno Reclaim moved and offers no delete', { timeout: 10 * 60_000 }, async ({ app }) => {
    assert.ok(outstanding.size, 'the Explore move ran first');
    const root = await openReclaim(app);
    const page = root.page();
    await goTo(root, 'recycle.title');
    const moved = root.locator('[data-test="recycle-moved"]');
    await moved.waitFor({ timeout: 60_000 });
    await root.getByRole('button', { name: t('recycle.oldest.showFor', { share }), exact: true }).click({ timeout: 60_000 });
    const detail = root.getByTestId('recycle-detail');
    await detail.locator('[data-test="recycle-share-moved"]').waitFor({ timeout: 60_000 });
    assert.match(await detail.locator('[data-test="recycle-share-moved"]').innerText(), /\d/, 'the share shows the moved size');
    await screenshot(page, 'recycle-page');
    // Nothing on the page selects or removes Recycle Bin items: emptying stays in DSM.
    assert.equal(await root.getByRole('checkbox').count(), 0, 'no item can be selected');
    assert.equal(await root.getByRole('button', { name: t('explore.basket.review'), exact: true }).count(), 0, 'no cleanup action');
    assert.ok((await root.locator('[data-test="recycle-dsm"]').innerText()).includes(t('recycle.hint.title')), 'the page says Syno Reclaim never empties it');
  });

  test('File Station restores the moved items to where they were', { timeout: 10 * 60_000 }, async ({ app }) => {
    assert.ok(outstanding.size, 'the Explore move ran first');
    const root = await openReclaim(app);
    const page = root.page();
    await restoreFromRecycleBin(page, share, [...outstanding]);
    for (const item of [...outstanding]) {
      const back = await tree(sharePath(share, item));
      sameFiles(back, before.get(item)!, `${item} is back whole`);
      // Restore renames on the same volume: the files are the very ones moved.
      const inodes = (entries: Entry[]) => entries.filter(entry => entry.type === 'f').map(entry => `${entry.path}\t${entry.inode}`);
      assert.deepEqual(inodes(back), inodes(before.get(item)!), `${item} keeps its files' inodes`);
      assert.deepEqual(files(await tree(sharePath(share, item, true))), [], `${item} left the Recycle Bin`);
      outstanding.delete(item);
    }
  });

  test('closing the page leaves a running move alone: it shows again, cancels, and reports its result', { timeout: 30 * 60_000 }, async ({ app }) => {
    let root = await openReclaim(app);
    const page = root.page();
    restorePage = page;
    for (const item of longItems) {
      assert.deepEqual(await tree(sharePath(share, item, true)), [], `${item} is not in the Recycle Bin yet`);
      before.set(item, await tree(sharePath(share, item)));
    }
    await exploreShare(root, share);
    await clearList(root);
    // Select the folders in the given order, opening each parent folder once.
    let shown = '';
    for (const item of longItems) {
      const parent = item.slice(0, item.lastIndexOf('/'));
      if (parent !== shown) {
        // Up through the breadcrumb, whose buttons are folder names, to the
        // deepest folder both share; then down through the rows' path buttons.
        const from = shown ? shown.split('/') : [];
        const to = parent.split('/');
        let common = 0;
        while (common < from.length && common < to.length && from[common] === to[common]) common += 1;
        if (shown) {
          const breadcrumb = root.getByLabel(t('explore.breadcrumb.label'), { exact: true });
          await breadcrumb.getByRole('button', { name: common ? to[common - 1] : share, exact: true }).click({ timeout: 60_000 });
        }
        let path = `/volume1/${share}${common ? `/${to.slice(0, common).join('/')}` : ''}`;
        for (const part of to.slice(common)) {
          path += `/${part}`;
          await root.getByRole('button', { name: path, exact: true }).click({ timeout: 60_000 });
        }
        shown = parent;
      }
      await root.getByRole('checkbox', { name: t('explore.basket.select', { name: item.split('/').at(-1)! }), exact: true }).check();
    }
    await root.getByRole('button', { name: t('explore.basket.review'), exact: true }).click();
    const dialog = await reviewDialog(page);
    await dialog.locator('[data-test="review-recycle"]').waitFor({ timeout: 10 * 60_000 });
    const known = new Set((await api('jobs.list')).jobs.map((job: any) => job.job_id));
    await dialog.getByRole('checkbox', { name: t('review.confirmCheck'), exact: true }).check();
    await dialog.getByRole('button', { name: t('review.confirmMove', { count: longItems.length }), exact: true }).click();
    for (const item of longItems) outstanding.add(item);
    const running = await poll(async () => (await api('jobs.list')).jobs.find((job: any) =>
      !known.has(job.job_id) && job.kind === 'cleanup-apply' && job.state === 'running'), 'the move to start', 120_000, 500);
    await screenshot(page, 'long-move-started');

    // The browser goes away mid-move: reload the DSM page and open Syno Reclaim again.
    const reloadedAt = Date.now();
    await page.reload({ waitUntil: 'domcontentloaded' });
    root = await openReclaim(app);
    const after = (await answer('jobs.list')).jobs.find((job: any) => job.job_id === running.job_id);
    console.log(`[reload] Syno Reclaim reopened ${Date.now() - reloadedAt} ms after the reload; the move is ${after?.state}, ${after?.cleanup?.items_done} of ${after?.cleanup?.items_total} items`);
    assert.equal(after?.state, 'running', 'the move kept running while the page was gone');
    const strip = root.getByRole('listitem').filter({ hasText: t('common.job.kind.cleanup-apply') }).first();
    await strip.waitFor({ timeout: 60_000 });
    await screenshot(page, 'long-move-job-strip');
    await strip.getByRole('button', { name: t('newScan.busy.view'), exact: true }).click();
    const following = page.getByRole('dialog', { name: t('review.applying.title') });
    await following.waitFor({ timeout: 60_000 });
    await following.getByText(new RegExp(t('review.applying.count', { done: '__', total: '__' }).replace(/[.*+?^${}()|[\]\\]/g, '\\$&').replace(/__/g, '[\\d,.\\s]+'))).first().waitFor({ timeout: 60_000 });
    await screenshot(page, 'long-move-progress-after-reload');

    // Cancel stops between items; the result names what moved, the rest stays.
    await following.getByRole('button', { name: t('review.applying.cancel'), exact: true }).click();
    const ended = await poll(async () => {
      const job = (await answer('jobs.list')).jobs.find((entry: any) => entry.job_id === running.job_id);
      return ['completed', 'partial', 'failed', 'cancelled', 'interrupted'].includes(job?.state) ? job : undefined;
    }, 'the move to end', 10 * 60_000, 2000);
    const result = page.getByRole('dialog').filter({ hasText: ended.state === 'cancelled' ? t('review.result.cancelledTitle') : t('review.result.completeTitle') });
    await result.waitFor({ timeout: 120_000 });
    await screenshot(page, 'long-move-result');
    const document = await answer('job.result', { job_id: running.job_id });
    console.log(`[reload] the move ended ${ended.state}: ${document.moved} moved of ${document.items}, ${document.moved_bytes} bytes`);
    assert.equal(ended.state, 'cancelled', 'the cancel stopped the move before its last item');
    assert.ok(document.moved >= 1 && document.moved < longItems.length, 'some items moved, the rest stayed');
    let moved = 0;
    for (const item of longItems) {
      const here = await tree(sharePath(share, item));
      if (here.length) {
        sameFiles(here, before.get(item)!, `${item} stayed whole`);
        assert.deepEqual(await tree(sharePath(share, item, true)), [], `${item} did not move`);
        outstanding.delete(item);
      } else {
        sameFiles(await tree(sharePath(share, item, true)), before.get(item)!, `${item} is whole in #recycle`);
        moved += 1;
      }
    }
    assert.equal(moved, document.moved, 'the result counts what moved');
    await result.getByRole('button', { name: t('common.close'), exact: true }).last().click();

    // Put back what moved, as File Station's Restore does.
    const back = longItems.filter(item => outstanding.has(item));
    await restoreFromRecycleBin(page, share, back);
    for (const item of back) {
      sameFiles(await tree(sharePath(share, item)), before.get(item)!, `${item} is back`);
      outstanding.delete(item);
    }
  });

  test('a shared folder without a Recycle Bin is deleted for good only once confirmed, and says where to turn it on', { timeout: 10 * 60_000 }, async ({ app }) => {
    const root = await openReclaim(app);
    const page = root.page();
    const untouched = await tree(sharePath(offShare, blocked));
    await exploreShare(root, offShare);
    await clearList(root);
    await root.getByRole('checkbox', { name: t('explore.basket.select', { name: blocked }), exact: true }).check();
    await root.getByRole('button', { name: t('explore.basket.review'), exact: true }).click();
    const dialog = await reviewDialog(page);
    const why = dialog.locator('[data-test="review-delete-bin-off"]');
    await why.waitFor({ timeout: 5 * 60_000 });
    const text = await why.innerText();
    assert.ok(text.includes(t('review.delete.binOff', { share: offShare })), 'it says the folder has no Recycle Bin and its items go for good');
    assert.ok(text.includes(t('review.delete.binOffEnable')), 'it says turning the Recycle Bin on moves them instead');
    assert.equal(await why.locator(`[data-testid="share-settings-${offShare}"]`).count(), 1, 'it opens the shared folder in Control Panel');
    const confirm = dialog.getByRole('button', { name: t('review.confirmDelete', { count: 1 }), exact: true });
    assert.ok(await confirm.isDisabled(), 'nothing goes before the box is ticked');
    await dialog.locator('[data-test="review-recheck"]').waitFor();
    await screenshot(page, 'no-recycle-bin');
    // The test data stays: close before confirming.
    await dialog.getByRole('button', { name: t('common.close'), exact: true }).last().click();
    await dialog.waitFor({ state: 'hidden', timeout: 30_000 });
    sameFiles(await tree(sharePath(offShare, blocked)), untouched, `${blocked} stays where it is`);
  });

  // ---- Duplicates ---------------------------------------------------------

  type Group = { group: { id: string; kind: string; name: { path: string }; size: number }; keeper: { path: { path: string } };
    in_scope: { path: { path: string }; role: string }[]; in_scope_total: number; removes: number };
  let chosen: Group | undefined;

  /** The share's big duplicate files, as the page's first page lists them. */
  async function fileGroups(scope: string, category?: string): Promise<{ total: number; items: Group[] }> {
    const answer = await api('duplicates.scope', { volume: '/volume1', path: `/volume1/${scope}`, list: 'files', limit: 200, ...(category ? { category } : {}) });
    return answer.groups;
  }

  /** A small file group near the top whose copies here all move and whose name is unique on the page. */
  function pick(groups: Group[]): Group | undefined {
    const names = groups.map(group => group.group.name.path);
    return groups.slice(0, 12)
      .filter(group => group.group.kind === 'file' && group.removes > 0 && group.removes === group.in_scope_total
        && group.in_scope.every(copy => copy.role === 'remove') && names.filter(name => name === group.group.name.path).length === 1)
      .sort((a, b) => a.group.size - b.group.size)[0];
  }

  /** Duplicates for one shared folder, analysed against the newest scan. */
  async function openDuplicates(root: Root, scope: string): Promise<void> {
    await goTo(root, 'duplicates.title');
    // Duplicates reopens where it was left: start from the volume.
    const crumb = root.getByLabel(t('duplicates.breadcrumb.label'), { exact: true }).getByRole('button', { name: 'volume1', exact: true });
    await crumb.waitFor({ timeout: 30_000 }).then(() => crumb.click(), () => undefined);
    const again = root.getByRole('button', { name: t('duplicates.analysis.again'), exact: true });
    const find = root.locator('[data-testid="duplicates-find"]');
    const link = root.locator(`[data-testid="duplicates-share-${scope}"]`);
    await link.or(again).or(find).first().waitFor({ timeout: 5 * 60_000 });
    if (await find.count()) await find.click();
    else if (await again.count()) await again.first().click();
    await link.waitFor({ timeout: 20 * 60_000 });
    await link.click();
    await root.locator('[data-testid="duplicates-lists"]').waitFor({ timeout: 60_000 });
  }

  function groupRow(root: Root, name: string) {
    return root.getByRole('row').filter({ has: root.page().getByRole('checkbox', { name: t('duplicates.table.selectRow', { name }), exact: true }) });
  }

  test('Duplicates lists a shared folder by list and kind, ticks a copy to keep and selects all at once', { timeout: 30 * 60_000 }, async ({ app }) => {
    const root = await openReclaim(app);
    const page = root.page();
    await openDuplicates(root, share);
    await screenshot(page, 'duplicates-share');
    const folders = (await api('duplicates.scope', { volume: '/volume1', path: `/volume1/${share}`, list: 'both', limit: 1 })).folders ?? { total: 0 };
    const groups = await fileGroups(share);
    const tabs = root.locator('[data-testid="duplicates-tabs"]');
    // Each list names its count in a pill after its title.
    const foldersTab = tabs.getByRole('tab', { name: t('duplicates.folders.title') });
    await foldersTab.waitFor();
    assert.ok((await foldersTab.innerText()).includes(String(folders.total)), 'the folders list shows its count');
    await tabs.getByRole('tab', { name: t('duplicates.files.title') }).click();
    chosen = pick(groups.items);
    assert.ok(chosen, `${share} lists a small duplicate file whose copies here all move`);
    await groupRow(root, chosen.group.name.path).waitFor({ timeout: 60_000 });

    // Kind: each filter lists only that kind's groups.
    const kinds = root.getByRole('tablist', { name: t('duplicates.filter.label'), exact: true });
    for (const [filter, category] of [['video', 'videos'], ['photo', 'photos'], ['music', 'music']] as const) {
      await kinds.getByRole('tab', { name: t(`duplicates.filter.${filter}`), exact: true }).click();
      const expected = await fileGroups(share, category);
      if (expected.items[0]) await groupRow(root, expected.items[0].group.name.path).waitFor({ timeout: 60_000 });
      else await root.getByText(t('duplicates.files.empty'), { exact: true }).waitFor({ timeout: 60_000 });
      // A row of another kind is not listed under this one.
      const names = new Set(expected.items.map(group => group.group.name.path));
      const other = groups.items.find(group => !names.has(group.group.name.path));
      if (other && expected.items.length < 200) {
        assert.equal(await root.getByRole('checkbox', { name: t('duplicates.table.selectRow', { name: other.group.name.path }), exact: true }).count(), 0,
          `${other.group.name.path} is not listed under ${filter}`);
      }
      if (filter === 'video') await screenshot(page, 'duplicates-videos');
    }
    await kinds.getByRole('tab', { name: t('duplicates.filter.all'), exact: true }).click();

    // Keep: a row opens its copies in a sheet; ticking a copy keeps it, unticking moves it again.
    const row = groupRow(root, chosen.group.name.path);
    await row.waitFor({ timeout: 60_000 });
    await row.getByRole('cell').last().click();
    const sheet = root.locator('[data-testid="keep-sheet"]');
    await sheet.waitFor({ timeout: 60_000 });
    const keepReply = () => page.waitForResponse(response => {
      try { return response.request().postDataJSON()?.method === 'duplicates.keep'; } catch { return false; }
    }, { timeout: 60_000 });
    const removing = sheet.locator('[data-testid="keep-copy-remove"]');
    await removing.first().waitFor({ timeout: 60_000 });
    const before = await removing.count();
    let reply = keepReply();
    await removing.first().getByRole('checkbox').click();
    assert.equal((await (await reply).json()).success, true, 'the kept copy is saved');
    await poll(async () => (await removing.count()) === before - 1 || undefined, 'the copy to show as kept', 60_000, 500);
    await screenshot(page, 'duplicates-kept');
    // Every copy here moved, so the ticked copy is the only one kept here; the
    // kept copy elsewhere stays, so it can be unticked again.
    reply = keepReply();
    await sheet.locator('[data-testid="keep-copy-keep"]').first().getByRole('checkbox').click();
    assert.equal((await (await reply).json()).success, true, 'the choice is undone');
    await poll(async () => (await removing.count()) === before || undefined, 'the copy to show as moved again', 60_000, 500);
    await page.keyboard.press('Escape');
    await sheet.waitFor({ state: 'hidden', timeout: 15_000 });

    // Select all: every group in the shared folder goes to the list in one go.
    await clearList(root);
    const selectAll = root.locator('[data-testid="duplicates-select-all"]');
    const started = Date.now();
    await selectAll.click();
    // Done when every group is in the list: the button turns itself off.
    await poll(async () => (await selectAll.isDisabled()) || undefined, 'Select all to finish', 60_000, 100);
    const unchecked = await root.locator('[data-testid="duplicates-grid-files"]').getByRole('row').getByRole('checkbox', { checked: false, disabled: false }).count();
    assert.equal(unchecked, 0, 'every listed group is selected');
    const selectMs = Date.now() - started;
    console.log(`[duplicates] Select all in ${share}: ${selectMs} ms for ${groups.total} file and ${folders.total} folder groups`);
    await screenshot(page, 'duplicates-select-all');
    assert.ok(selectMs < 10_000, `Select all fills the list within 10 s (took ${selectMs} ms)`);
    await clearList(root);
  });

  test('Duplicates moves the copies to remove into the Recycle Bin and keeps the kept copy', { timeout: 20 * 60_000 }, async ({ app }) => {
    assert.ok(chosen, 'the Duplicates list test chose a group');
    const group = chosen;
    const copies = group.in_scope.map(copy => copy.path.path.slice(`/volume1/${share}/`.length));
    for (const copy of copies) before.set(copy, await tree(sharePath(share, copy)));
    const keeper = group.keeper.path.path;
    const root = await openReclaim(app);
    const page = root.page();
    await openDuplicates(root, share);
    await root.locator('[data-testid="duplicates-tabs"]').getByRole('tab').nth(1).click();
    await clearList(root);
    await root.getByRole('checkbox', { name: t('duplicates.table.selectRow', { name: group.group.name.path }), exact: true }).check({ timeout: 60_000 });
    await root.getByRole('button', { name: t('explore.basket.review'), exact: true }).click();
    const dialog = await reviewDialog(page);
    const summary = dialog.locator('[data-test="review-duplicates"]');
    await summary.waitFor({ timeout: 5 * 60_000 });
    // The review names it from the volume: /<shared folder>/….
    assert.ok((await summary.innerText()).includes(keeper.replace(/^\/volume\d+/, '')), 'the review names the copy it keeps');
    await dialog.locator('[data-test="review-kept-checked"]').waitFor();
    await screenshot(page, 'duplicates-review');

    const result = await confirmMove(page, dialog, copies.length);
    for (const copy of copies) outstanding.add(copy);
    await result.locator('[data-test="review-moved"]').waitFor();
    await screenshot(page, 'duplicates-moved');
    for (const copy of copies) {
      assert.deepEqual(await tree(sharePath(share, copy)), [], `${copy} left its place`);
      sameFiles(await poll(async () => {
        const recycled = await tree(sharePath(share, copy, true));
        return recycled.length ? recycled : undefined;
      }, `${copy} in the Recycle Bin`, 2 * 60_000, 2000), before.get(copy)!, `${copy} is in #recycle`);
    }
    const keeperShare = keeper.split('/')[2]!;
    const kept = await tree(sharePath(keeperShare, keeper.slice(`/volume1/${keeperShare}/`.length)));
    assert.equal(kept.length, 1, 'the kept copy stays');
    await result.getByRole('button', { name: t('common.close'), exact: true }).last().click();
    await result.waitFor({ state: 'hidden', timeout: 15_000 }).catch(async () => {
      await page.keyboard.press('Escape');
      await result.waitFor({ state: 'hidden', timeout: 15_000 });
    });

    await restoreFromRecycleBin(page, share, copies);
    for (const copy of copies) {
      sameFiles(await tree(sharePath(share, copy)), before.get(copy)!, `${copy} is back after File Station Restore`);
      outstanding.delete(copy);
    }
  });

  test('Duplicates mark a shared folder without a Recycle Bin as deleted for good and let its duplicates be selected', { timeout: 20 * 60_000 }, async ({ app }) => {
    const root = await openReclaim(app);
    const page = root.page();
    await openDuplicates(root, offShare);
    const tabs = root.locator('[data-testid="duplicates-tabs"]');
    await tabs.getByRole('tab').nth(1).click();
    const groups = (await fileGroups(offShare)).items;
    const group = groups.find(entry => entry.removes > 0);
    assert.ok(group, `${offShare} lists a duplicate file to remove`);
    const box = root.getByRole('checkbox', { name: t('duplicates.table.selectRow', { name: group.group.name.path }), exact: true }).first();
    await box.waitFor({ timeout: 60_000 });
    assert.ok(await box.isEnabled(), 'its rows can be selected');
    assert.ok(await root.getByText(t('duplicates.row.deletes'), { exact: false }).first().isVisible(), 'the row says its copies go for good');
    assert.ok(await root.locator('[data-testid="duplicates-select-all"]').isEnabled(), 'and so can Select all');
    await screenshot(page, 'duplicates-no-recycle-bin');
    // The volume overview marks it too.
    await root.getByLabel(t('duplicates.breadcrumb.label'), { exact: true }).getByRole('button', { name: 'volume1', exact: true }).click();
    const badge = root.locator(`[data-testid="duplicates-bin-${offShare}"]`);
    await badge.waitFor({ timeout: 60_000 });
    assert.equal((await badge.innerText()).trim(), t('duplicates.shares.binOffDeletes'));
    await screenshot(page, 'duplicates-volume-recycle-bins');
    for (const copy of group.in_scope) {
      const item = copy.path.path.slice(`/volume1/${offShare}/`.length);
      assert.equal((await tree(sharePath(offShare, item))).length, 1, `${item} stays where it is`);
    }
  });

  // ---- System partition -----------------------------------------------------

  test('system partition cleanup still deletes for good: no Recycle Bin, and it says there is no undo', { timeout: 20 * 60_000 }, async ({ app }) => {
    const root = await openReclaim(app);
    const page = root.page();
    const applyCalls: string[] = [];
    // Planning only: the browser may never send an apply of the system partition.
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
      // Only a NAS whose system partition needs it offers its cleanup; never fill it for a test.
      const level = (await api('volumes.list')).system_partition?.level;
      if (!level || level === 'ok') {
        console.log(`[rootfs] not exercised: this NAS offers no system partition cleanup (level ${level ?? 'unknown'})`);
        return;
      }
      await root.locator('button.volume-card').filter({ hasText: t('rootfs.name') }).click();
      await root.locator('.rootfs-movable-button').click({ timeout: 90_000 });
      const dialog = page.getByRole('dialog', { name: t('rootfsClean.title'), exact: true });
      await dialog.waitFor();
      const destinations = dialog.getByTestId('rootfs-destination');
      const search = destinations.getByRole('searchbox');
      if (await search.count()) {
        await search.fill(share);
        for (const collapsed of await destinations.locator('button[aria-expanded="false"]').all()) await collapsed.click();
      }
      await destinations.getByRole('radio', { name: new RegExp(share) }).check();
      await dialog.getByRole('button', { name: t('rootfsClean.action.next'), exact: true }).click();
      await dialog.getByRole('checkbox', { name: t('rootfsClean.review.check'), exact: true }).waitFor({ timeout: 5 * 60_000 });
      await dialog.getByText(t('rootfsClean.review.noUndoTitle'), { exact: false }).first().waitFor();
      const text = await dialog.innerText();
      assert.ok(!text.includes(t('review.recycle.title')) && !text.includes(t('review.confirmCheck')), 'the system partition never goes to a Recycle Bin');
      await screenshot(page, 'rootfs-no-undo');
      await dialog.getByRole('button', { name: t('common.close'), exact: true }).first().click();
    } finally {
      await page.unroute('**/cgi/api.cgi', guard);
    }
    assert.deepEqual(applyCalls, [], 'the browser never sent a system partition apply');
  });
});
