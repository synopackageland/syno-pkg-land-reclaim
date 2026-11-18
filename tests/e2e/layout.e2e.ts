// Regression: at 1440 px the Packages names and 管理位置 are cut off; later the
// column headers and the Explore treemap labels too (German 「Freigeb…」「Container Mana…」);
// and the Explore table scrolled sideways, cutting off 「Wo verwalten」.
import assert from 'node:assert/strict';
import { test } from '@e2e-dev/web';
import { APP_ID, goTo, openReclaim } from './support/reclaim.ts';

type Root = Awaited<ReturnType<typeof openReclaim>>;

/**
 * Table cell texts that overflow their box (ellipsis or clipped). A CellText line is measured on its
 * own box, [data-ui-cell-primary] or [data-ui-cell-secondary]: the text inside it may sit in an inline
 * span whose clientWidth is 0, which a leaf-only check misses ("Hyper Backup（映像與…"). Other cell
 * text and the column headers are measured on their leaf elements.
 */
async function truncated(root: Root): Promise<string[]> {
  await root.getByRole('table').first().waitFor({ timeout: 30_000 });
  return root.getByRole('table').first().evaluate(table => {
    const lines = [...table.querySelectorAll('[data-ui-cell-primary], [data-ui-cell-secondary]')];
    const leaves = [...table.querySelectorAll('td *, [role=cell] *, th *, [role=columnheader] *')]
      .filter(element => element.children.length === 0 && !lines.some(line => line.contains(element)));
    return [...lines, ...leaves]
      // Visually hidden labels (a 1 px clip) are not shown text.
      .filter(element => (element as HTMLElement).innerText?.trim() && element.clientWidth > 1 && element.scrollWidth > element.clientWidth + 1)
      .map(element => (element as HTMLElement).innerText.trim());
  });
}

/**
 * Treemap labels that overflow their tile, or end in an ellipsis although the name fits: a label
 * starts 8 px into its tile and keeps 8 px from its right edge. The name is the tile title's start.
 */
async function truncatedLabels(root: Root): Promise<string[]> {
  // Each tile is a g[role=button]; the g that holds them all would pair the first label with another tile's rect.
  return root.locator('svg.explore-treemap').first().evaluate(map => [...map.querySelectorAll('g[role=button]')].flatMap(tile => {
    const [label, rect, title] = [tile.querySelector('text'), tile.querySelector('rect'), tile.querySelector('title')];
    if (!label || !rect || !title) return [];
    const name = /^(.*?)(\s?:\s|：)/.exec(title.textContent ?? '')?.[1] ?? '';
    const probe = label.cloneNode() as SVGTextElement;
    probe.textContent = name;
    label.after(probe);
    const full = probe.getComputedTextLength();
    probe.remove();
    const room = rect.width.baseVal.value - 16;
    const text = label.textContent ?? '';
    return (text.endsWith('…') && full <= room) || label.getComputedTextLength() > room ? [`treemap: ${text}`] : [];
  }));
}

/** How far the table's scrolling box scrolls sideways, in px; 0 when its columns fit. */
async function sidewaysScroll(root: Root): Promise<number> {
  return root.getByRole('table').first().evaluate(table => {
    for (let box = table.parentElement; box; box = box.parentElement) {
      if (/auto|scroll/.test(getComputedStyle(box).overflowX)) return Math.max(0, box.scrollWidth - box.clientWidth);
    }
    return 0;
  });
}

test('[P4] at 1440 px the Packages and Explore tables, headers and treemap labels show in full, with no sideways scroll', { session: 'dsm', timeout: 180_000 }, async ({ app }) => {
  const root = await openReclaim(app);
  const page = root.page();
  await page.setViewportSize({ width: 1440, height: 900 });
  // A block body: maximize() returns the ExtJS window, which cannot cross back to the test.
  await page.evaluate(id => { (window as any).SYNO.SDS.AppMgr.getByAppName(id)[0].window.maximize(); }, APP_ID);
  try {
    await goTo(root, 'packages.title');
    const packages = await truncated(root);
    const packagesScroll = await sidewaysScroll(root);
    await goTo(root, 'explore.title');
    const explore = [...await truncated(root), ...await truncatedLabels(root)];
    const exploreScroll = await sidewaysScroll(root);
    assert.deepEqual({ packages, explore, packagesScroll, exploreScroll }, { packages: [], explore: [], packagesScroll: 0, exploreScroll: 0 },
      'no table text, header or treemap label is cut off, and no table scrolls sideways, at 1440 px');
  } finally {
    await page.evaluate(id => { (window as any).SYNO.SDS.AppMgr.getByAppName(id)[0].window.restore(); }, APP_ID).catch(() => undefined);
  }
});
