// Reclaim-specific helpers on top of the shared DSM module: UI strings from
// the app's own catalogs, CGI readback, sidebar navigation, and the owned
// fixture each scenario creates on the NAS and removes afterwards.
import { spawn } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { readFile, readdir } from 'node:fs/promises';
import { createRequire } from 'node:module';
import { resolve } from 'node:path';
import type { Request } from 'playwright-core';
import { dsmPage, openApp } from '@synopackageland/cli/e2e';
import { dsmReady } from './readiness.ts';

type Page = ReturnType<typeof dsmPage>;
type Locator = ReturnType<Page['locator']>;
type App = Parameters<typeof openApp>[0];

export const APP_ID = 'SYNO.SDS.SynoReclaim.Instance';
export const PORTAL_PATH = '/synoreclaim/';
export const ADMIN_ONLY = 'admin_required';

// ---- UI strings -----------------------------------------------------------

type Message = string | Record<string, string>;
const i18nRoot = resolve(import.meta.dirname, '../../../ui/src/i18n');
const dsmLanguages: Record<string, string> = { cht: 'zh-TW', chs: 'zh-CN', enu: 'en', jpn: 'ja', krn: 'ko', ger: 'de', fre: 'fr', ita: 'it', spn: 'es' };
let messages: Record<string, Message> | undefined;
let language = 'en';

async function catalog(lang: string): Promise<Record<string, Message>> {
  const merged: Record<string, Message> = {};
  for (const folder of await readdir(i18nRoot, { withFileTypes: true })) {
    if (!folder.isDirectory()) continue;
    for (const file of ['en', lang]) {
      Object.assign(merged, JSON.parse(await readFile(resolve(i18nRoot, folder.name, `${file}.json`), 'utf8').catch(() => '{}')));
    }
  }
  return merged;
}

/** Loads the catalog of the language the signed-in DSM session shows. */
export async function loadStrings(page: Page = dsmPage()): Promise<string> {
  const dsm = await page.evaluate(() => (window as any).SYNO?.SDS?.Session?.lang ?? 'enu');
  const lang = dsmLanguages[dsm] ?? 'en';
  messages = await catalog(lang);
  language = lang;
  return lang;
}

/** The app's message for `key`, with `{name}` placeholders filled. */
export function t(key: string, params: Record<string, string | number> = {}): string {
  if (!messages) throw new Error('Call loadStrings() first');
  const message = messages[key];
  if (message === undefined) throw new Error(`No UI string ${key}`);
  const text = typeof message === 'string' ? message : message.other ?? Object.values(message)[0];
  return text.replace(/\{(\w+)\}/g, (whole, name) => name in params ? String(params[name]) : whole);
}

// The SDK's own copy, from the @synopackageland/sdk that ui/ installs (pnpm install in ui/ first):
// the namespace is its i18n folder, such as authorization (the Gate) or delegate (ExecutorApproval).
const sdk = createRequire(resolve(import.meta.dirname, '../../../ui/package.json'));

function sdkCatalog(namespace: string, lang: string): Record<string, string> {
  return JSON.parse(readFileSync(sdk.resolve(`@synopackageland/sdk/i18n/${namespace}/${lang}.json`), 'utf8'));
}

/** The SDK's message for `key` in the language loadStrings() found, for Reclaim. */
export function sdkText(key: string, namespace = 'authorization'): string {
  let copy: Record<string, string>;
  try { copy = sdkCatalog(namespace, language); } catch { copy = sdkCatalog(namespace, 'en'); }
  const text = copy[key];
  if (text === undefined) throw new Error(`No SDK string ${namespace}/${key}`);
  return text.replace(/\{package\}/g, 'Syno Reclaim');
}

// ---- CGI readback ---------------------------------------------------------

export type CgiReply = { success: boolean; data?: any; error?: { code: string; message?: string } };

/** Calls the app's CGI from the signed-in page; the relative URL resolves under the portal alias too. */
export async function cgi(method: string, params: Record<string, unknown> = {}, page: Page = dsmPage()): Promise<CgiReply> {
  return page.evaluate(async ({ method, params }) => {
    const token = (window as any).SYNO?.SDS?.Session?.SynoToken;
    const response = await fetch('webman/3rdparty/SynoReclaim/cgi/api.cgi', { method: 'POST', credentials: 'same-origin',
      headers: { 'Content-Type': 'application/json', ...(token ? { 'X-SYNO-TOKEN': token } : {}) },
      body: JSON.stringify({ method, params }) });
    return response.json();
  }, { method, params });
}

export type CallTiming = { call: string; startMs: number; endMs: number; ms: number; outcome: string };

/** The method a request to the app's CGI or a DSM WebAPI names, or undefined for any other request. */
function callName(url: string, body: string | null): string | undefined {
  if (url.includes('/cgi/api.cgi')) {
    try { return `cgi ${JSON.parse(body ?? '{}').method}`; } catch { return 'cgi ?'; }
  }
  if (!url.includes('/webapi/')) return undefined;
  const form = new URLSearchParams(body ?? '');
  const query = new URL(url).searchParams;
  const field = (name: string) => form.get(name) ?? query.get(name) ?? '?';
  return `webapi ${field('api')}.${field('method')}`;
}

/**
 * Records when each call to the app's CGI and DSM's WebAPI starts and ends, in ms from now, so a
 * slow first open can be laid to the call that held it. `stop()` ends the recording.
 */
export function traceCalls(page: Page): { stop(): CallTiming[] } {
  const started = Date.now();
  const open = new Map<Request, { call: string; startMs: number }>();
  const done: CallTiming[] = [];
  const begin = (request: Request) => {
    const call = callName(request.url(), request.postData());
    if (call) open.set(request, { call, startMs: Date.now() - started });
  };
  const end = (outcome: (request: Request) => Promise<string> | string) => async (request: Request) => {
    const call = open.get(request);
    if (!call) return;
    open.delete(request);
    const endMs = Date.now() - started;
    done.push({ ...call, endMs, ms: endMs - call.startMs, outcome: await outcome(request) });
  };
  const finished = end(async request => String((await request.response())?.status() ?? '?'));
  const failed = end(request => request.failure()?.errorText ?? 'failed');
  page.on('request', begin);
  page.on('requestfinished', finished);
  page.on('requestfailed', failed);
  return {
    stop() {
      page.off('request', begin);
      page.off('requestfinished', finished);
      page.off('requestfailed', failed);
      const now = Date.now() - started;
      const pending = [...open.values()].map(call => ({ ...call, endMs: now, ms: now - call.startMs, outcome: 'pending' }));
      return [...done, ...pending].sort((a, b) => a.startMs - b.startMs);
    },
  };
}

/**
 * The data of a successful call; anything else fails the step with the error code. Right after a
 * cleanup the executor refuses even reads with ROOT_BUSY for a while, so a readback waits that out.
 */
export async function api(method: string, params: Record<string, unknown> = {}): Promise<any> {
  let reply = await cgi(method, params);
  for (const end = Date.now() + 60_000; reply.error?.code === 'ROOT_BUSY' && Date.now() < end; reply = await cgi(method, params)) {
    await new Promise(next => setTimeout(next, 2000));
  }
  if (!reply.success) throw new Error(`${method} failed: ${reply.error?.code} ${reply.error?.message ?? ''}`);
  return reply.data;
}

// ---- Window and navigation ------------------------------------------------

const guarded = new WeakSet<Page>();

/** Closes DSM notification cards whenever one would block an action: they arrive after the desktop shows. */
export async function closeNotificationsOnArrival(page: Page = dsmPage()): Promise<void> {
  if (guarded.has(page)) return;
  guarded.add(page);
  const close = page.locator('.v-notification-panel.show .close-btn');
  await page.addLocatorHandler(close.first(), async () => {
    // Closing cards detach during their fade. Click the current DOM snapshot
    // together instead of starting a locator action that can chase a removed card.
    await close.evaluateAll(buttons => {
      for (const button of buttons) if (button instanceof HTMLElement) button.click();
    });
  });
}

/** Opens the Reclaim window with DSM notifications out of the way and its strings loaded; returns the app root. */
export async function openReclaim(app: App): Promise<Locator> {
  const window = await dsmReady(() => openApp(app, APP_ID, 180_000));
  // DSM may restore another window after AppLaunch. Focus only our window;
  // never close another App or change its settings to prepare a test.
  await window.page().evaluate(id => {
    (globalThis.window as any).SYNO.SDS.AppMgr.getByAppName(id)[0].window.toFront();
  }, APP_ID);
  window.page().setDefaultTimeout(120_000);
  window.page().setDefaultNavigationTimeout(300_000);
  await closeNotificationsOnArrival();
  await loadStrings();
  // AppShell's actions and sidebar are siblings of main in the current SDK.
  return window.locator('.app-root').first();
}

/** The SDK translates the landmark name; identify our navigation by a destination. */
export function appNavigation(page: Page = dsmPage()): Locator {
  return page.getByRole('navigation').filter({
    has: page.getByRole('button', { name: t('overview.title'), exact: true }),
  });
}

/** Clicks a sidebar item and waits for its page heading. */
export async function goTo(root: Locator, titleKey: string): Promise<void> {
  // The installed SDK localizes the navigation landmark too. Identify it by
  // its translated destination instead of requiring an English landmark name.
  const navigation = root.page().getByRole('navigation').filter({
    has: root.page().getByRole('button', { name: t(titleKey), exact: true }),
  });
  await navigation.getByRole('button', { name: t(titleKey), exact: true }).click({ timeout: 180_000 });
  await root.page().getByRole('heading', { level: 1, name: t(titleKey), exact: true }).waitFor({ timeout: 180_000 });
}

export const terminalStates = ['completed', 'partial', 'failed', 'cancelled', 'interrupted', 'superseded'];

/** The job `jobId` as jobs.list reports it now. */
export async function job(jobId: string): Promise<any> {
  return (await api('jobs.list')).jobs.find((entry: any) => entry.job_id === jobId);
}

/** Starts a fast scan of one shared folder through the New scan dialog; returns the new job. */
export async function startShareScan(root: Locator, share: string): Promise<any> {
  await goTo(root, 'overview.title');
  const before = new Set((await api('jobs.list')).jobs.map((entry: any) => entry.job_id));
  await root.getByRole('button', { name: t('shell.newScan'), exact: true }).click();
  const dialog = root.page().getByRole('dialog', { name: t('newScan.title') });
  await dialog.getByRole('radio', { name: t('newScan.scope.shares') }).check();
  // The picker pages long share lists; each row is named "<share> <size>" once whitespace is normalized.
  await dialog.getByPlaceholder(t('common.picker.search')).fill(share);
  const row = new RegExp(`^${share.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}\\s`);
  await dialog.getByRole('group', { name: t('newScan.shares.title') }).getByRole('checkbox', { name: row }).check();
  await dialog.getByRole('radio', { name: t('newScan.mode.fast') }).check();
  await dialog.getByRole('button', { name: t('newScan.startShares', { count: 1 }), exact: true }).click();
  return poll(async () => (await api('jobs.list')).jobs.find((entry: any) => !before.has(entry.job_id) && entry.kind === 'scan'),
    'the new scan job', 30_000, 500);
}

/** Polls `check` until it returns a value, for readbacks of background work. */
export async function poll<T>(check: () => Promise<T | undefined>, what: string, timeoutMs: number, intervalMs = 3000): Promise<T> {
  for (const end = Date.now() + timeoutMs; Date.now() < end; await new Promise(next => setTimeout(next, intervalMs))) {
    const value = await check();
    if (value !== undefined) return value;
  }
  throw new Error(`Timed out after ${timeoutMs} ms waiting for ${what}`);
}

// ---- Package lifecycle -----------------------------------------------------

/** The SSH target the run was explicitly given; there is no host default. */
export function dsmSsh(): string {
  const host = process.env.DSM_E2E_SSH;
  if (!host) throw new Error('DSM_E2E_SSH must name the authorized acceptance host');
  return host;
}

/** Runs `synopkg <action> SynoReclaim` on the NAS; only these three fixed actions exist. */
export async function synopkg(action: 'start' | 'stop' | 'is_onoff'): Promise<string> {
  const ssh = dsmSsh();
  const child = spawn('ssh', ['-o', 'BatchMode=yes', ssh, '/usr/syno/bin/synopkg', action, 'SynoReclaim'], { stdio: ['ignore', 'pipe', 'pipe'] });
  let stdout = '';
  child.stdout.on('data', chunk => { stdout += chunk; });
  const code = await new Promise<number>(accept => child.on('close', status => accept(status ?? 1)));
  if (action !== 'is_onoff' && code !== 0) throw new Error(`synopkg ${action} SynoReclaim failed (${code})`);
  return stdout.trim();
}

export const packageRunning = async (): Promise<boolean> => /is turned on/i.test(await synopkg('is_onoff'));

// ---- Owned fixtures -------------------------------------------------------

const ownedPath = /^\/volume\d+\/[A-Za-z0-9_.-]+\/(#recycle\/)?reclaim-e2e-[a-z0-9-]+(\/[A-Za-z0-9_-][A-Za-z0-9_.-]*)*$/;

/** Runs a fixed script over SSH; the script receives only the owned fixture path. */
async function onNas(script: string, path: string): Promise<string> {
  if (!ownedPath.test(path)) throw new Error(`Not an owned fixture path: ${path}`);
  const ssh = dsmSsh();
  const child = spawn('ssh', ['-o', 'BatchMode=yes', ssh, 'sh', '-s', '--', path], { stdio: ['pipe', 'pipe', 'pipe'] });
  let stdout = '', stderr = '';
  child.stdout.on('data', chunk => { stdout += chunk; });
  child.stderr.on('data', chunk => { stderr += chunk; });
  child.stdin.end(script);
  const code = await new Promise<number>(accept => child.on('close', status => accept(status ?? 1)));
  if (code !== 0) throw new Error(`NAS fixture script failed (${code}): ${stderr.split('\n').filter(line => !line.startsWith('**')).join(' ').trim()}`);
  return stdout;
}

export type Fixture = { share: string; name: string; root: string; recycled: string; keep: string; remove: string; again: string };

/** A uniquely named fixture for this run, under the scratch share. */
export function fixtureFor(label: string, share = process.env.E2E_FIXTURE_SHARE || 'share'): Fixture {
  const name = `reclaim-e2e-${Date.now().toString(36)}-${label}`;
  const root = `/volume1/${share}/${name}`;
  return { share, name, root, recycled: `/volume1/${share}/#recycle/${name}`, keep: `${root}/keep`, remove: `${root}/remove`, again: `${root}/again` };
}

/** keep/ (1 MiB), remove/ (3 × 4 MiB) and again/ (4 × 1 MiB) of random bytes, plus a 2 MiB recycled item, owned by admin. */
export async function createFixture(fixture: Fixture): Promise<void> {
  // Random bytes so no extent is shared with anything else on the volume.
  await onNas(`set -eu
root="$1"
mkdir "$root" "$root/keep" "$root/remove" "$root/again"
head -c 1048576 /dev/urandom > "$root/keep/control.bin"
for n in 1 2 3; do head -c 4194304 /dev/urandom > "$root/remove/part-$n.bin"; done
for n in 1 2 3 4; do head -c 1048576 /dev/urandom > "$root/again/item-$n.bin"; done
chown -R admin:users "$root"`, fixture.root);
  await onNas(`set -eu
mkdir "$1"
head -c 2097152 /dev/urandom > "$1/recycled.bin"
chown -R admin:users "$1"
# Btrfs allocates the extents at commit; a scan before that reads 0 B for every file.
sync`, fixture.recycled);
}

/** Lists which fixture entries exist, as `find` prints them. */
export async function fixtureEntries(path: string): Promise<string[]> {
  return (await onNas('find "$1" 2>/dev/null || true', path)).split('\n').filter(Boolean).sort();
}

export async function removeFixture(fixture: Fixture): Promise<void> {
  for (const path of [fixture.root, fixture.recycled]) await onNas('rm -rf -- "$1"', path);
}

/** Change business namespace and data outside Reclaim state while its scan is paused. */
export async function rewriteBusinessFixture(fixture: Fixture): Promise<void> {
  await onNas(`set -eu
head -c 524288 /dev/urandom > "$1/keep/control.bin"
mv "$1/remove/part-1.bin" "$1/remove/renamed.bin"
rm "$1/again/item-1.bin"
sync`, fixture.root);
}
