// Package stop and start with the full page open, then the first open
// after the restart. The package is running before and after.
import assert from 'node:assert/strict';
import { mkdirSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { afterAll, describe } from 'e2e';
import { test } from '@e2e-dev/web';
import { openDesktop, openPortal } from '@synopackageland/cli/e2e';
import { appNavigation, APP_ID, PORTAL_PATH, goTo, loadStrings, packageRunning, poll, sdkText, synopkg, t, traceCalls, type CallTiming } from './support/reclaim.ts';

type LoadingSample = { at: number; found: { frame: string; window: string; html: string; text: string }[] };

/** In a frame: the innermost visible elements whose text says Loading, with the DSM window around them. */
function findLoading(): { window: string; html: string; text: string }[] {
  const hits: { window: string; html: string; text: string }[] = [];
  for (const element of document.querySelectorAll<HTMLElement>('body *')) {
    const own = [...element.childNodes].filter(node => node.nodeType === Node.TEXT_NODE).map(node => node.textContent ?? '').join('');
    const label = `${own} ${element.getAttribute('aria-label') ?? ''} ${element.getAttribute('title') ?? ''}`;
    if (!/Loading/.test(label) || !element.getClientRects().length) continue;
    const window = element.closest<HTMLElement>('.x-window, .sds-window');
    const title = window?.querySelector<HTMLElement>('.x-window-header-text, .x-window-header .x-panel-header-text')?.innerText ?? '';
    hits.push({ window: window ? `${window.id} "${title}"` : 'outside any DSM window', html: element.outerHTML.slice(0, 600), text: label.trim() });
  }
  return hits;
}

/**
 * When the page's first session.get started against the end of the window's first scheduler
 * readback (EventScheduler, TaskScheduler), or undefined when either is missing from the trace.
 * The readback is the run of scheduler calls each starting within 100 ms of the run's end so far;
 * a later check, after the pages show, starts a new run.
 */
function sessionAgainstScheduler(calls: CallTiming[]): { sessionStartMs: number; schedulerEndMs: number } | undefined {
  const session = calls.find(call => call.call === 'cgi session.get');
  const scheduler = calls.filter(call => /^webapi SYNO\.Core\.(EventScheduler|TaskScheduler)\./.test(call.call));
  if (!session || !scheduler.length) return undefined;
  let schedulerEndMs = scheduler[0].endMs;
  for (const call of scheduler.slice(1)) {
    if (call.startMs > schedulerEndMs + 100) break;
    schedulerEndMs = Math.max(schedulerEndMs, call.endMs);
  }
  return { sessionStartMs: session.startMs, schedulerEndMs };
}

// Not serial: P2 fails until the SDK update and must not skip R1; each test leaves the package running.
describe('package stop and start', { session: 'dsm' }, () => {
  afterAll(async () => { if (!await packageRunning()) await synopkg('start'); });

  // Regression, with the SDK 0.10.5 copy: a stopped package reads as stopped,
  // never as needing approval, and the page recovers by itself once it runs. DSM closes an app
  // window when its package stops and sends the portal page of a stopped package to its sign-in,
  // so the full page opens while the package runs and then moves to Explore once it is stopped.
  // Since CLI 0.10.8 the window rejects APP_STOPPED at once, so either SDK copy may say so: the
  // Gate's title, or ExecutorApproval's own words when it takes over first.
  test('[P2] a stopped package shows Syno Reclaim 已停止 and the page recovers after start', { timeout: 5 * 60_000 }, async ({ app }) => {
    assert.ok(await packageRunning(), 'the package runs before the test');
    const page = await openPortal(app, PORTAL_PATH);
    await loadStrings(page);
    const main = page.locator('main').first();
    await goTo(main, 'overview.title');
    await synopkg('stop');
    const stoppedAt = Date.now();
    const stoppedCopy = [sdkText('authorization.stopped.title'), sdkText('appStopped', 'delegate')];
    const stopped = page.getByText(stoppedCopy[0], { exact: true }).or(page.getByText(stoppedCopy[1], { exact: true }));
    const shown = stopped.first().waitFor({ timeout: 90_000 }).then(() => Date.now() - stoppedAt, () => undefined);
    // The page may already have replaced the app with the stopped copy; then there is no sidebar to use,
    // and the click would wait out its 30 s timeout before the copy is timed.
    await Promise.race([goTo(main, 'explore.title').catch(() => undefined), shown]);
    const shownAfter = await shown;
    const stoppedWithin5s = shownAfter !== undefined && shownAfter <= 5000;
    const approvalForm = await page.locator('#delegate-password').count()
      + await page.getByText(sdkText('authorization.title'), { exact: true }).count();
    const appError = await page.getByText(t('code.api.backend_unavailable')).count();
    await synopkg('start');
    // No reload: the open page must come back by itself.
    const recovered = await poll(async () => await appNavigation(page).isVisible()
      && await stopped.count() === 0 && await page.getByText(t('code.api.backend_unavailable')).count() === 0 || undefined,
    'the page to recover', 90_000, 1000).then(() => true, () => false);
    console.log(`[P2] ${JSON.stringify({ shownAfter, approvalForm, appError, recovered })}`);
    assert.deepEqual({ stoppedWithin5s, shownAfter, approvalForm, appError, recovered },
      { stoppedWithin5s: true, shownAfter, approvalForm: 0, appError: 0, recovered: true },
      `within 5 s of the stop the page says "${stoppedCopy.join('" or "')}", shows no approval form and no error of its own, and recovers after start`);
  });

  // Regression: an English "Loading…" for ~6.7 s on the first open after an
  // upgrade, from an unknown source. A restart stands in for the upgrade. Every 300 ms the test saves a
  // screenshot and every element showing Loading with the frame and DSM window it sits in, under
  // E2E_TRACE_DIR (default .e2e/r1-trace), so the report can name the source. It also logs when each
  // CGI and WebAPI call starts and ends, and when the app's navigation shows (Overview took
  // 9.6 s, the approval form 14.9 s), into calls.json and the log. Since build 1565 ExecutorApproval
  // preloads the pages: a window that still reads the scheduler back must not hold the
  // page's own session.get until that readback ends. An executor that reports its own readback
  // ready leaves no scheduler call at all, which the log says.
  test('[R1] the first open after a restart shows no English Loading…', { timeout: 3 * 60_000 }, async ({ app }) => {
    await synopkg('stop');
    await synopkg('start');
    const page = await openDesktop(app);
    const lang = await loadStrings(page);
    assert.notEqual(lang, 'en', 'DSM shows a non-English language');
    const dir = resolve(process.env.E2E_TRACE_DIR || '.e2e/r1-trace', new Date().toISOString().replace(/[:.]/g, '-'));
    mkdirSync(dir, { recursive: true });
    const samples: LoadingSample[] = [];
    const calls = traceCalls(page);
    const launched = Date.now();
    let shownAt: number | undefined;
    await page.evaluate(id => (window as any).SYNO.SDS.AppLaunch(id), APP_ID);
    for (let n = 0; Date.now() - launched < 12_000; n++, await page.waitForTimeout(300)) {
      const at = Date.now() - launched;
      const found: LoadingSample['found'] = [];
      for (const frame of page.frames()) {
        const hits = await frame.evaluate(findLoading).catch(() => []);
        for (const hit of hits) found.push({ frame: frame === page.mainFrame() ? 'top' : frame.url(), ...hit });
      }
      await page.screenshot({ path: resolve(dir, `${String(n).padStart(3, '0')}-${at}ms.png`) }).catch(() => undefined);
      samples.push({ at, found });
      if (shownAt === undefined && await appNavigation(page).isVisible()) shownAt = at;
    }
    const timings = calls.stop();
    writeFileSync(resolve(dir, 'calls.json'), JSON.stringify({ shownAt, calls: timings }, null, 1));
    for (const call of timings) console.log(`[R1 call] ${call.startMs}–${call.endMs} ms (${call.ms} ms) ${call.call} ${call.outcome}`);
    console.log(`[R1] the app's navigation showed ${shownAt === undefined ? 'not within 12 s' : `after ${shownAt} ms`}`);
    const preload = sessionAgainstScheduler(timings);
    console.log(`[R1] ${preload ? `session.get started at ${preload.sessionStartMs} ms, the scheduler readback ended at ${preload.schedulerEndMs} ms` : 'no scheduler readback or no session.get within the trace'}`);
    writeFileSync(resolve(dir, 'samples.json'), JSON.stringify(samples, null, 1));
    const seen = samples.filter(sample => sample.found.length);
    console.log(`[R1] ${seen.length} of ${samples.length} samples show Loading; trace in ${dir}`);
    assert.deepEqual(seen.map(sample => ({ at: sample.at, sources: sample.found.map(hit => `${hit.frame} ${hit.window} ${hit.html.slice(0, 160)}`) })), [],
      'the window never shows English loading text');
    assert.ok(timings.some(call => call.call === 'cgi session.get'), 'the page asks for its session within the trace');
    assert.ok(!preload || preload.sessionStartMs < preload.schedulerEndMs,
      `the page asks for its session while the scheduler is read back, not after it (${JSON.stringify(preload)})`);
  });
});
