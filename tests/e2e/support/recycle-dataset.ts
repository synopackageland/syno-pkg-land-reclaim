// The duplicates test data (reclaimdup-* shared folders) the Recycle Bin cleanup
// tests move items of, and put back: NAS readback over SSH, File Station's own
// Restore, and screenshots. Nothing here deletes, creates or edits a file.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdirSync } from 'node:fs';
import { resolve } from 'node:path';
import { dsmPage } from '@synopackageland/cli/e2e';
import { dsmSsh } from './reclaim.ts';

type Page = ReturnType<typeof dsmPage>;

const shareName = /^reclaimdup-[a-z0-9-]+$/;
// Item paths inside a share: the dataset's names, no quotes or shell syntax.
const itemPath = /^[A-Za-z0-9 _.()-]+(\/[A-Za-z0-9 _.()-]+)*$/;

/** A reclaimdup-* shared folder the run was told to use; there is no default. */
export function datasetShare(variable: string): string {
  const share = process.env[variable];
  assert.ok(share, `Set ${variable} to the reclaimdup-* shared folder to use`);
  assert.match(share, shareName, `${variable} must name a reclaimdup-* shared folder`);
  return share;
}

/** The item paths a variable lists, comma separated, inside a shared folder. */
export function datasetItems(variable: string): string[] {
  const items = (process.env[variable] ?? '').split(',').map(item => item.trim()).filter(Boolean);
  assert.ok(items.length, `Set ${variable} to the items to use`);
  for (const item of items) assert.match(item, itemPath, `${variable}: ${item} is not a plain path in the shared folder`);
  return items;
}

/** `/volume1/<share>/<item>`, or the same place in the share's Recycle Bin. */
export function sharePath(share: string, item: string, recycled = false): string {
  assert.match(share, shareName);
  assert.match(item, itemPath);
  return `/volume1/${share}/${recycled ? '#recycle/' : ''}${item}`;
}

async function onNas(script: string): Promise<string> {
  const child = spawn('ssh', ['-o', 'BatchMode=yes', dsmSsh(), 'sh', '-s'], { stdio: ['pipe', 'pipe', 'pipe'] });
  let stdout = '', stderr = '';
  child.stdout.on('data', chunk => { stdout += chunk; });
  child.stderr.on('data', chunk => { stderr += chunk; });
  child.stdin.end(script);
  const code = await new Promise<number>(accept => child.on('close', status => accept(status ?? 1)));
  assert.equal(code, 0, `NAS readback failed: ${stderr.trim()}`);
  return stdout;
}

const quoted = (path: string) => {
  assert.match(path, /^\/volume1\/reclaimdup-[a-z0-9-]+\/(#recycle\/)?[A-Za-z0-9 _.()/-]+$/, `Not a dataset path: ${path}`);
  return `'${path}'`;
};

export type Entry = { type: string; size: number; inode: number; path: string };

/** Every file and folder under each path (the path itself first), as lstat sees it; [] when absent. */
export async function tree(path: string): Promise<Entry[]> {
  const out = await onNas(`p=${quoted(path)}
if test -e "$p" || test -L "$p"; then find "$p" -path '*/@eaDir' -prune -o -printf '%y\\t%s\\t%i\\t%P\\n'; fi`);
  return out.split('\n').filter(Boolean).map(line => {
    const [type, size, inode, rest] = line.split('\t');
    return { type, size: Number(size), inode: Number(inode), path: rest };
  }).sort((a, b) => a.path.localeCompare(b.path));
}

/** The files of a tree by relative path and size: what has to arrive in, and come back from, the Recycle Bin. */
export const files = (entries: Entry[]) => entries.filter(entry => entry.type === 'f').map(entry => `${entry.path}\t${entry.size}`);

/** Whether DSM's Recycle Bin of a shared folder is on, and for administrators only, as synoshare reports it. */
export async function recycleBin(share: string): Promise<{ enabled: boolean; adminOnly: boolean }> {
  assert.match(share, shareName);
  const out = await onNas(`/usr/syno/sbin/synoshare --get '${share}'`);
  const flag = (name: string) => new RegExp(`${name}\\.+\\[(yes|no)\\]`).exec(out)?.[1] === 'yes';
  return { enabled: flag('RecycleBin'), adminOnly: flag('RecycleBinAdminOnly') };
}

type WebApi = { success: boolean; data?: any; error?: { code: number } };

async function webapi(page: Page, api: string, method: string, version: number, params: Record<string, unknown>): Promise<WebApi> {
  return page.evaluate(async ({ api, method, version, params }) => {
    const token = (window as any).SYNO?.SDS?.Session?.SynoToken;
    const body = new URLSearchParams({ api, method, version: String(version) });
    for (const [key, value] of Object.entries(params)) body.set(key, JSON.stringify(value));
    const response = await fetch('webapi/entry.cgi', { method: 'POST', credentials: 'same-origin',
      headers: { 'Content-Type': 'application/x-www-form-urlencoded', ...(token ? { 'X-SYNO-TOKEN': token } : {}) }, body });
    return response.json();
  }, { api, method, version, params });
}

/**
 * Restores items from a shared folder's Recycle Bin the way File Station's Restore does (DSM 7.2.1+):
 * CopyMove with restore_from_recycle_bin into the same place without #recycle, never overwriting.
 */
export async function restoreFromRecycleBin(page: Page, share: string, items: string[]): Promise<void> {
  const byParent = new Map<string, string[]>();
  for (const item of items) {
    const parent = item.includes('/') ? item.slice(0, item.lastIndexOf('/')) : '';
    byParent.set(parent, [...byParent.get(parent) ?? [], `/${share}/#recycle/${item}`]);
  }
  for (const [parent, paths] of byParent) {
    const started = await webapi(page, 'SYNO.FileStation.CopyMove', 'start', 3, {
      path: paths, dest_folder_path: `/${share}${parent ? `/${parent}` : ''}`,
      remove_src: true, overwrite: false, restore_from_recycle_bin: true, accurate_progress: true,
    });
    assert.ok(started.success && started.data?.taskid, `File Station Restore started (${started.error?.code ?? ''})`);
    const end = Date.now() + 5 * 60_000;
    for (;;) {
      const status = await webapi(page, 'SYNO.FileStation.CopyMove', 'status', 3, { taskid: started.data.taskid });
      assert.ok(status.success, `File Station Restore status (${status.error?.code ?? ''})`);
      if (status.data?.finished) break;
      assert.ok(Date.now() < end, 'File Station Restore finished within 5 minutes');
      await new Promise(next => setTimeout(next, 1000));
    }
  }
}

let shot = 0;
/** Saves a numbered screenshot of the DSM page under E2E_SHOT_DIR (default .e2e/recycle-delete). */
export async function screenshot(page: Page, name: string): Promise<void> {
  const dir = resolve(process.env.E2E_SHOT_DIR || '.e2e/recycle-delete');
  mkdirSync(dir, { recursive: true });
  shot += 1;
  await page.screenshot({ path: resolve(dir, `${String(shot).padStart(2, '0')}-${name}.png`) });
}
