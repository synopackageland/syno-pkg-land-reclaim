// Explicit opt-in only: this file is intentionally outside the local *.test.mjs gate.
import assert from 'node:assert/strict';
import { after, before, describe, it } from 'node:test';
import { cancelCtlScan, ctl, waitForTinyShareScan } from '../e2e/support/ctl.ts';
import { createFeatureFixture, featureExists, featureFixture, removeFeatureFixture, required,
  type FeatureFixture } from '../e2e/support/feature-fixture.ts';

describe('real NAS ctl CLI main features', { concurrency: false, timeout: 30 * 60_000 }, () => {
  let fixture: FeatureFixture;
  let scanId: string | undefined;
  before(async () => {
    assert.equal(required('RECLAIM_REAL_NAS'), '1', 'Explicit real-NAS opt-in');
    fixture = featureFixture();
    await createFeatureFixture(fixture);
    if (process.env.E2E_FAILED_ONLY === '1') {
      const accepted = await ctl('scans.start', { targets: [fixture.sharePath], mode: 'fast', snapshots: true });
      assert.equal(accepted.accepted, true, 'Prepare only the tiny owned share report for failed read cases');
      scanId = accepted.job_id;
      assert.ok(scanId);
      await waitForTinyShareScan(scanId, fixture.sharePath);
    }
  });
  after(async () => {
    if (scanId) await cancelCtlScan(scanId);
    if (fixture) await removeFeatureFixture(fixture);
  });

  it('authorization get proves the real executor is ready', async () => {
    const result = await ctl('ctl.authorization.status');
    assert.equal(result.state, 'ready');
  });
  it('volumes list returns scannable capacity', async () => {
    const result = await ctl('ctl.volumes.list');
    assert.ok(result.volumes.some((v: any) => v.capabilities?.scan && v.total_bytes > 0));
  });
  it('scans start accepts one fixture scan without waiting', async () => {
    const result = await ctl('scans.start', { targets: [fixture.sharePath], mode: 'fast', snapshots: true });
    // Preserve a returned ID even if another assertion fails, so teardown can stop our scan.
    if (result.accepted === true) scanId = result.job_id;
    assert.equal(result.accepted, true, 'No unrelated active scan is reused');
    assert.match(scanId!, /^[A-Za-z0-9-]+$/);
  });
  it('scans get reaches completed for the original job', async () => {
    assert.ok(scanId, 'The start test must have admitted an owned scan');
    await waitForTinyShareScan(scanId, fixture.sharePath);
  });
  it('jobs result contains the completed scan report and real entries', async () => {
    assert.ok(scanId);
    const result = await ctl('ctl.job.result', { job_id: scanId });
    assert.equal(result.job_id, scanId);
    assert.ok(JSON.stringify(result).includes('report'), 'A result document contains report evidence');
    assert.equal(result.error_count ?? 0, 0);
  });
  it('history list contains the exact completed scan ID', async () => {
    assert.ok(scanId);
    const result = await ctl('ctl.history.list');
    assert.equal(result.scans.find((s: any) => s.job_id === scanId)?.outcome, 'completed');
  });
  it('overview get returns nonzero filesystem capacity', async () => {
    const result = await ctl('ctl.overview.get', { volume: fixture.volume });
    assert.ok(result.results.some((r: any) => r.space?.df?.total_bytes > 0));
  });
  it('snapshots list returns measured fixture snapshot inventory', async () => {
    const result = await ctl('ctl.snapshots.list', { target: process.env.E2E_SNAPSHOT_SHARE_PATH || fixture.sharePath });
    assert.ok(result.results.some((r: any) => r.shares?.some((s: any) => s.snapshots?.length >= (process.env.E2E_SNAPSHOT_SHARE_PATH ? 3 : 0))));
    assert.equal(result.error_count ?? 0, 0);
  });
  it('recycle list accounts for the test-created recycled item', async () => {
    const result = await ctl('ctl.recycle.list', { target: fixture.sharePath, oldest: 100 });
    assert.ok(result.shares.some((s: any) => s.items >= 1 && s.bytes >= 2097152));
  });
  it('cache get exposes filesystem cache statistics', async () => {
    const result = await ctl('ctl.cache.status', { volume: fixture.volume });
    assert.ok(result.filesystems.length > 0);
    assert.ok(result.filesystems.every((f: any) => Number.isFinite(f.bytes) && f.bytes >= 0));
  });
  it('diagnostics get runs real environment checks', async () => {
    const result = await ctl('ctl.doctor.run', { volume: fixture.volume });
    assert.ok(result.checks.length > 0);
    assert.ok(result.checks.every((c: any) => typeof c.status === 'string'));
  });
  it('space explain returns filesystem accounting', async () => {
    const result = await ctl('ctl.why_not_freed.get', { volume: fixture.volume, processes: false });
    assert.ok(result.results.length > 0);
    assert.equal(result.error_count ?? 0, 0);
  });
  it('paths simulate measures the exact scanned share while preserving the control file', async () => {
    // Read-only simulation requires a report of exactly this selection. A
    // whole-share scan cannot stand in for a separately selected child file.
    const result = await ctl('ctl.simulate.get', { paths: [fixture.sharePath] });
    assert.ok(result.results.length > 0);
    assert.ok(result.results.some((r: any) => r.combined?.physical > 0));
    assert.equal(result.error_count ?? 0, 0);
    assert.equal(await featureExists(`${fixture.root}/control.bin`, fixture), true);
  });
  it('top list returns ranked report rows with byte values', async () => {
    const result = await ctl('ctl.top.list', { target: fixture.sharePath, kind: 'all', by: 'reclaimable', limit: 20 });
    assert.ok(result.results.some((r: any) => r.entries?.some((entry: any) => entry.metrics?.physical > 0)));
  });
  // Last: clearing cache must not invalidate earlier report-backed assertions.
  it('cache clear empties App cache and preserves fixture data and history, including an empty cache', async () => {
    const historyBefore = (await ctl('ctl.history.list')).scans.map((s: any) => s.job_id);
    const before = await ctl('ctl.cache.status', { volume: fixture.volume });
    const cleared = await ctl('ctl.cache.clear', { volume: fixture.volume });
    assert.ok(cleared.filesystems.length > 0);
    assert.ok(cleared.filesystems.every((f: any) => Number.isInteger(f.cleared) && f.cleared >= 0));
    if (before.filesystems.some((f: any) => f.bytes > 0)) {
      assert.ok(cleared.filesystems.some((f: any) => f.cleared > 0), 'Existing cache units were removed');
    } else {
      assert.ok(cleared.filesystems.every((f: any) => f.cleared === 0), 'Clearing an empty cache is idempotent');
    }
    const after = await ctl('ctl.cache.status', { volume: fixture.volume });
    assert.ok(after.filesystems.every((f: any) => f.bytes === 0));
    assert.equal(await featureExists(`${fixture.root}/control.bin`, fixture), true);
    const historyAfter = (await ctl('ctl.history.list')).scans.map((s: any) => s.job_id);
    assert.ok(historyBefore.every((id: string) => historyAfter.includes(id)), 'Existing reports/history are retained');
  });
});
