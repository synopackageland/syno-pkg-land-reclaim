import { test } from 'node:test';
import assert from 'node:assert/strict';
import { dsmReady } from './readiness.ts';

test('DSM readiness tolerates transient network failure before returning the ready page', async () => {
  let calls = 0;
  const page = await dsmReady(async () => {
    if (++calls === 1) throw new Error('network ERR_CONNECTION_RESET');
    return 'ready';
  }, 10_000);
  assert.equal(page, 'ready');
  assert.equal(calls, 2);
});

test('DSM readiness never retries a product assertion or authorization refusal', async () => {
  let calls = 0;
  await assert.rejects(dsmReady(async () => {
    calls++;
    throw new Error('ADMIN_REQUIRED');
  }), /ADMIN_REQUIRED/);
  assert.equal(calls, 1);
});
