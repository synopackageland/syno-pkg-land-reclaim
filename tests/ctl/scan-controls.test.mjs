import {test} from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {runCtl} from '@synopackageland/cli/dist/ctl/cli.js';
import {CtlClient} from '@synopackageland/cli/dist/ctl/service.js';

test('jobs pause/resume, Fast/Gentle mode and cancel dispatch through real Rust handler results', async () => {
  const description = JSON.parse(readFileSync('generated/synopkgland-api.json'));
  const steps = JSON.parse(readFileSync(process.env.RECLAIM_CTL_RESULTS || '.tmp/ctl-results.json'))
    .filter(row => row.scenario === 'scan-controls');
  assert.equal(steps.length, 10, 'generate the scan-control corpus with the Rust App tests first');
  const original = CtlClient.prototype.discover;
  CtlClient.prototype.discover = async () => ({apps:[description], installed:[{
    id:description.payload.package.id, version:description.payload.package.version,
    aliases:description.payload.aliases,
  }], diagnostics:[], cacheHits:0});
  const host = {name:'fixture'};
  let cursor = 0, output = [], last, requestFailure;
  const io = {isTTY:false, out:line => output.push(line),
    store:{list:() => [host], targets:() => [host], defaultName:() => host.name,
      get:() => host, session:async () => ({})},
    requestFor:() => async (_path, body) => {
      const expected = steps[cursor++];
      try {
        assert.ok(expected, 'unexpected extra request');
        assert.equal(body.method, expected.method);
        assert.deepEqual(body.params, expected.params);
      } catch (error) { requestFailure = error.message; throw error; }
      last = expected;
      return {success:true, data:expected.result};
    }};
  async function run(args) {
    output = [];
    const before = cursor;
    await runCtl(['reclaim', ...args, '--host', 'fixture', '--json'], io);
    assert.equal(process.exitCode || 0, 0, JSON.stringify({output, requestFailure}));
    assert.equal(cursor, before + 1, 'command dispatches exactly once');
    return last;
  }
  async function state(expectedState, profile) {
    const row = await run(['jobs', 'list']);
    assert.equal(row.result.jobs.length, 1);
    const scan = row.result.jobs[0];
    assert.equal(scan.job_id, 'scan-one');
    assert.equal(scan.state, expectedState);
    assert.equal(scan.profile, profile);
    assert.deepEqual(scan.progress, {entries:123, items:456}, 'controls preserve checkpoint progress');
  }
  try {
    await run(['jobs', 'pause', '--job-id', 'scan-one', '--yes']);
    await state('paused', 'fast');
    const resumed = await run(['jobs', 'resume', '--job-id', 'scan-one', '--yes', '--no-wait']);
    assert.equal(resumed.result.accepted, true);
    assert.equal(resumed.result.job_id, 'scan-one');
    assert.equal(resumed.worker_running, true, 'the App started the resumed worker');
    await state('running', 'low');
    for (const [mode, profile] of [['fast', 'fast'], ['gentle', 'low']]) {
      await run(['jobs', 'mode', '--job-id', 'scan-one', '--mode', mode, '--yes']);
      await state('running', profile);
    }
    const cancelled = await run(['jobs', 'cancel', '--job-id', 'scan-one', '--yes']);
    assert.equal(cancelled.worker_running, false, 'the App stopped the resumed executor worker');
    await state('cancelled', 'low');
    assert.equal(cursor, steps.length);
  } finally {
    CtlClient.prototype.discover = original;
    process.exitCode = 0;
  }
});

test('ctl accepts changed-unit resume and retains job totals plus honest reread counters', async () => {
  const description = JSON.parse(readFileSync('generated/synopkgland-api.json'));
  const steps = JSON.parse(readFileSync(process.env.RECLAIM_CTL_RESULTS || '.tmp/ctl-results.json'))
    .filter(row => row.scenario === 'live-writes');
  assert.equal(steps.length, 4);
  const original = CtlClient.prototype.discover;
  CtlClient.prototype.discover = async () => ({apps:[description], installed:[{
    id:description.payload.package.id,version:description.payload.package.version,aliases:description.payload.aliases,
  }],diagnostics:[],cacheHits:0});
  const host = {name:'fixture'};
  let cursor = 0, output = [], last;
  const io = {isTTY:false,out:line => output.push(line),
    store:{list:() => [host],targets:() => [host],defaultName:() => host.name,get:() => host,session:async () => ({})},
    requestFor:() => async (_path, body) => {
      const step = steps[cursor++];
      assert.equal(body.method, step.method);
      assert.deepEqual(body.params, step.params);
      last = step.result;
      return {success:true,data:last};
    }};
  async function run(args) {
    output = [];
    await runCtl(['reclaim',...args,'--host','fixture','--json'],io);
    assert.equal(process.exitCode || 0,0,JSON.stringify(output));
    return last;
  }
  try {
    await run(['jobs','pause','--job-id','scan-written','--yes']);
    const paused = (await run(['jobs','list'])).jobs[0];
    assert.equal(paused.state,'paused');
    const admitted = await run(['jobs','resume','--job-id','scan-written','--yes','--no-wait']);
    assert.equal(admitted.job_id,paused.job_id);
    const resumed = (await run(['jobs','list'])).jobs[0];
    assert.equal(resumed.state,'running');
    assert.equal(resumed.job_id,paused.job_id);
    assert.equal(resumed.progress.entries,paused.progress.entries);
    assert.ok(resumed.progress.items > paused.progress.items);
    assert.ok(resumed.progress.bytes_read > paused.progress.bytes_read);
    assert.deepEqual(resumed.progress.restart,{subvolume_id:256,entries:12,items:24,bytes_read:768});
    assert.equal(cursor,steps.length,'no new scan or full-volume completion polling');
  } finally { CtlClient.prototype.discover = original; process.exitCode = 0; }
});
