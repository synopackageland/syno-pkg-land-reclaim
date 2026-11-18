// Application Portal on DSM, HTTP and HTTPS ports. The DSM wrapper supplies
// the browser token; the pinned Rust CGI requires it independently of the cookie.
// Verify the actual signed-in App and tokenless refusal without assuming which
// DSM bootstrap endpoint supplies the token.
import assert from 'node:assert/strict';
import { spawn, type ChildProcess } from 'node:child_process';
import { request } from 'node:https';
import { connect, createServer } from 'node:net';
import { afterAll, beforeAll } from 'e2e';
import { test } from '@e2e-dev/web';
import { dsmPage, openDesktop, openPortal } from '@synopackageland/cli/e2e';
import { appNavigation, PORTAL_PATH, cgi, dsmSsh, loadStrings, poll, sdkText, t } from './support/reclaim.ts';

const forwards: Record<'http' | 'https', number> = { http: 0, https: 0 };
let ssh: ChildProcess | undefined;

const freePort = () => new Promise<number>((accept, reject) => {
  const server = createServer().once('error', reject);
  server.listen(0, '127.0.0.1', () => { const port = (server.address() as { port: number }).port; server.close(() => accept(port)); });
});
const portOpen = (port: number) => new Promise<boolean>(accept => {
  const socket = connect({ host: '127.0.0.1', port }).once('connect', () => { socket.destroy(); accept(true); }).once('error', () => accept(false));
});

beforeAll(async () => {
  forwards.http = await freePort();
  forwards.https = await freePort();
  ssh = spawn('ssh', ['-N', '-o', 'BatchMode=yes', '-o', 'ExitOnForwardFailure=yes',
    '-L', `127.0.0.1:${forwards.http}:127.0.0.1:80`, '-L', `127.0.0.1:${forwards.https}:127.0.0.1:443`,
    dsmSsh()], { stdio: ['ignore', 'ignore', 'inherit'] });
  for (const end = Date.now() + 15_000; !(await portOpen(forwards.https)); await new Promise(next => setTimeout(next, 100))) {
    if (Date.now() > end || ssh.exitCode !== null) throw new Error('The SSH forwards to ports 80 and 443 did not open.');
  }
});
afterAll(() => { ssh?.kill('SIGTERM'); });

async function expectPortalApp(): Promise<void> {
  const page = dsmPage();
  // Only the signed-in page has a SynoToken, its own or the one it recovered.
  await poll(() => page.evaluate(() => (window as any).SYNO?.SDS?.Session?.SynoToken ? true : undefined).catch(() => undefined),
    'the signed-in portal page', 60_000, 500);
  await loadStrings(page);
  const navigation = appNavigation(page);
  // SDK 0.10.5: a check that cannot succeed is no longer called temporary.
  const temporary = page.getByText(sdkText('authorization.unavailable.body'), { exact: true });
  await navigation.or(temporary).first().waitFor({ timeout: 60_000 });
  assert.equal(await temporary.count(), 0, 'the portal page does not call its failed authorization check temporary');
  await navigation.waitFor({ timeout: 60_000 });
  // A fresh portal login can expose its token before DSM replaces the sign-in language.
  // Read the settled session language once the app has rendered.
  await loadStrings(page);
  await page.getByRole('heading', { level: 1, name: t('overview.title'), exact: true }).waitFor({ timeout: 30_000 });
  assert.equal((await cgi('authorization.status')).data?.state, 'ready', 'the portal page reaches the CGI');
}

test('the portal alias opens the full-page app on the DSM port', { session: 'dsm', timeout: 120_000 }, async ({ app }) => {
  await openPortal(app, PORTAL_PATH);
  await expectPortalApp();
});

test('the portal alias on port 80 obtains a DSM token and refuses CGI requests without it', { session: 'dsm', timeout: 180_000 }, async ({ app }) => {
  await openDesktop(app);
  const page = dsmPage();
  const portal = `http://127.0.0.1:${forwards.http}${PORTAL_PATH}`;
  // DSM's current wrapper may supply the token without the legacy login.cgi
  // recovery request. Verify the real token and App, not a particular bootstrap URL.
  await page.goto(portal);
  await expectPortalApp();
  assert.equal(await page.getByRole('dialog', { name: sdkText('authorization.credential.title') }).count(), 0,
    'no sign-in prompt once the DSM wrapper supplies the token');
  const bare = await page.evaluate(async () => (await fetch('webman/3rdparty/SynoReclaim/cgi/api.cgi', { method: 'POST', credentials: 'same-origin',
    headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ method: 'authorization.status', params: {} }) })).json());
  // The pinned native Rust runtime reserves session_required for missing DSM credentials.
  assert.equal(bare.success, false, 'the CGI refuses the tokenless request');
  assert.equal(bare.error?.code, 'session_required', 'the CGI on port 80 requires DSM credentials');
});

/** One HTTPS request to the 443 forward with the session cookies; DSM's certificate is self-signed. */
function viaHttps(path: string, cookie: string, body?: string, token?: string): Promise<{ status: number; text: string }> {
  return new Promise((accept, reject) => {
    const outgoing = request({ host: '127.0.0.1', port: forwards.https, path, method: body ? 'POST' : 'GET', rejectUnauthorized: false,
      headers: { cookie, origin: `https://127.0.0.1:${forwards.https}`, ...(body ? { 'content-type': 'application/json' } : {}), ...(token ? { 'x-syno-token': token } : {}) } }, response => {
      let text = '';
      response.setEncoding('utf8').on('data', chunk => { text += chunk; }).on('end', () => accept({ status: response.statusCode ?? 0, text }));
    });
    outgoing.once('error', reject).end(body);
  });
}

test('the portal alias serves the app on port 443, and its CGI with the session token', { session: 'dsm', timeout: 120_000 }, async ({ app }) => {
  const page = await openDesktop(app);
  const cookie = (await page.context().cookies()).map(entry => `${entry.name}=${entry.value}`).join('; ');
  const token = await page.evaluate(() => (window as any).SYNO.SDS.Session.SynoToken as string);
  const html = await viaHttps(PORTAL_PATH, cookie);
  assert.equal(html.status, 200, 'GET /synoreclaim/ on 443');
  assert.match(html.text, /SynoReclaim/, 'the page loads the Reclaim bundle');
  // DSM serves /webman only on its own ports; under the alias the app calls the CGI relative to /synoreclaim/.
  const cgiPath = `${PORTAL_PATH}webman/3rdparty/SynoReclaim/cgi/api.cgi`;
  const body = JSON.stringify({ method: 'authorization.status', params: {} });
  assert.equal(JSON.parse((await viaHttps(cgiPath, cookie, body, token)).text).data?.state, 'ready', 'the CGI answers on 443 with the session token');
  // A portal page with the DSM port's session has no SynoToken: the CGI asks for a password sign-in.
  assert.equal(JSON.parse((await viaHttps(cgiPath, cookie, body)).text).error?.code, 'session_required', 'the CGI on 443 asks a page without the token to sign in again');
});
