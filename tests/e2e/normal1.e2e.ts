// normal1 at the permission boundary: no app in the main menu, no window, the
// CGI refuses every method, and the portal alias asks for a sign-in.
import assert from 'node:assert/strict';
import { afterAll, beforeAll } from 'e2e';
import { test } from '@e2e-dev/web';
import { openDesktop, openPortal } from '@synopackageland/cli/e2e';
import { ADMIN_ONLY, APP_ID, PORTAL_PATH, cgi, createFixture, fixtureEntries, fixtureFor, removeFixture } from './support/reclaim.ts';

const fixture = fixtureFor('normal1');
beforeAll(() => createFixture(fixture));
afterAll(() => removeFixture(fixture));

test('normal1 has no Reclaim in the main menu and cannot open its window', { session: 'dsm-normal1', timeout: 120_000 }, async ({ app }) => {
  const page = await openDesktop(app);
  const apps = await page.evaluate(() => (window as any).SYNO.SDS.AppUtil.getApps() as string[]);
  assert.ok(apps.length > 0 && !apps.includes(APP_ID), 'the main menu lists apps but not Reclaim');
  await page.evaluate(id => (window as any).SYNO.SDS.AppLaunch(id), APP_ID);
  await page.waitForTimeout(3000);
  const opened = await page.evaluate(id => ((window as any).SYNO.SDS.AppMgr.getByAppName(id) || []).some((instance: any) => instance.window?.isVisible?.()), APP_ID);
  assert.equal(opened, false, 'launching Reclaim opens no window');
});

test('the CGI refuses normal1 every method and leaves the fixture alone', { session: 'dsm-normal1', timeout: 120_000 }, async ({ app }) => {
  await openDesktop(app);
  const calls: [string, Record<string, unknown>][] = [
    ['overview.get', {}], ['volumes.list', {}], ['authorization.status', {}], ['history.list', {}],
    ['cleanup.plan', { paths: [fixture.remove] }], ['scan.start', { targets: [{ path: `/volume1/${fixture.share}` }], mode: 'fast' }],
  ];
  for (const [method, params] of calls) {
    const reply = await cgi(method, params);
    assert.equal(reply.success, false, `${method} is refused`);
    assert.equal(reply.error?.code, ADMIN_ONLY, `${method} answers ${ADMIN_ONLY}`);
  }
  assert.equal((await fixtureEntries(fixture.remove)).length, 4, 'the fixture is untouched');
});

test('the portal alias asks normal1 to sign in instead of showing the app', { session: 'dsm-normal1', timeout: 120_000 }, async ({ app, browser }) => {
  // DSM gates the portal on the app privilege before the app loads, so the app's own not-admin state never shows here.
  // Known DSM behavior: DSM's launcher shows a native "not authorized" alert before any app JS
  // loads, so the app cannot replace it. The suite accepts that alert, logs it and fails on any other;
  // the engine fails an attempt on any alert nobody handles.
  const alerts: string[] = [];
  await browser.onDialog(dialog => { alerts.push(dialog.message); return dialog.accept(); });
  const page = await openPortal(app, PORTAL_PATH);
  await page.locator('input[name="username"]').waitFor({ timeout: 60_000 });
  console.log(`[portal] DSM alerts: ${JSON.stringify(alerts)}`);
  assert.ok(alerts.length > 0 && alerts.every(text => /not authorized/i.test(text)),
    `DSM alerts only that normal1 may not use the service (${alerts.join(' | ')})`);
  assert.equal(await page.getByRole('navigation', { name: 'Application navigation' }).count(), 0, 'the app does not render');
});
