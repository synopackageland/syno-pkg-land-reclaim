import {test} from 'node:test';
import Ajv from 'ajv';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {inspectApiDescription} from '@synopackageland/cli/dist/standalone/api-description-inspect.js';
import {operationInput, confirmOperation, requestPlan} from '@synopackageland/cli/dist/ctl/input.js';
import {listView, runCtl} from '@synopackageland/cli/dist/ctl/cli.js';
import {CtlClient} from '@synopackageland/cli/dist/ctl/service.js';

const effects = {
  'job.pause':'write', 'job.resume':'write', 'job.cancel':'write', 'job.mode':'write',
  'cleanup.plan':'write', 'rootfs.plan':'write', 'cleanup.apply':'destructive',
  'rootfs.apply':'destructive', 'cache.clear':'destructive', 'session.get':'read',
  'setup.cleanup':'write', 'doctor.run':'read', 'why_not_freed.get':'read',
  'watch_free.sample':'read', 'support_bundle.get':'read',
};

test('outcome schemas retain cache clear counts and overview df metrics', () => {
  const d=JSON.parse(readFileSync('generated/synopkgland-api.json')).payload.commands;
  const ajv=new Ajv({strict:false});
  for(const [id,value] of [
    ['ctl.cache.clear',{command:'cache.clear',schema_version:1,filesystems:[{cleared:0,bytes:0}]}],
    ['ctl.overview.get',{command:'overview.get',schema_version:1,results:[{space:{df:{total_bytes:4096,used_bytes:1024,free_bytes:3072,available_bytes:2048}}}]}],
  ]) assert.equal(ajv.validate(d[id].resultSchema,value),true,id+': '+JSON.stringify(ajv.errors));
});
async function description() {
  const d = JSON.parse(readFileSync('generated/synopkgland-api.json'));
  const registry = Object.fromEntries(Object.entries(d.payload.bindings).map(([name,op])=>[name,op.permission]));
  return {description:d,registry};
}

test('every common operation is explicit and remains administrator-only', async () => {
  const {description:d, registry} = await description();
  assert.deepEqual(inspectApiDescription(JSON.stringify(d), d.payload.package, d.sha256, registry), []);
  assert.equal(d.payload.commands['scans.start'].flow.type, 'job');
  for (const [id, effect] of Object.entries(effects)) {
    const op = d.payload.commands['ctl.'+id];
    assert.ok(op, id);
    assert.equal(op.effect, effect, id);
    assert.equal(op.permission, 'administrators', id);
    assert.equal(op.flow.start || op.flow.method, 'ctl.' + id);
    if (op.effect !== 'read') assert.equal(op.ai, 'draft');
  }
  for (const [id, op] of Object.entries(d.payload.commands)) {
    if (op.flow.type === 'job') assert.equal(op.flow.timeoutMs, undefined, id);
    const params = operationInput(op, ['--input', '-'], JSON.stringify(op.examples[0]));
    assert.throws(() => operationInput(op, ['--input','-'], JSON.stringify({...params, root:true})), id);
    if (op.effect !== 'read') assert.throws(() => confirmOperation(op, false, false, requestPlan(d,id,params)), id);
  }
  for (const [id, key] of [['cleanup.apply','confirm_permanent'],['rootfs.apply','confirm_no_restore']]) {
    const op = d.payload.commands['ctl.'+id];
    for (const params of [{plan_id:'../plan', [key]:true}, {plan_id:op.examples[0].plan_id, [key]:false}, {plan_id:op.examples[0].plan_id}]) {
      assert.throws(() => operationInput(op,['--input','-'],JSON.stringify(params)));
    }
  }
});

test('real ctl list, dry-run, missing confirmation and execution use the descriptor', async () => {
  const {description:d} = await description();
  const original = CtlClient.prototype.discover;
  CtlClient.prototype.discover = async () => ({apps:[d],installed:[{id:d.payload.package.id,version:d.payload.package.version,aliases:d.payload.aliases}],diagnostics:[],cacheHits:0});
  const host={name:'fixture'};
  try {
    let requests=[]; let output=[];
    const io={isTTY:false,out:line=>output.push(line),store:{list:()=>[host],targets:()=>[host],defaultName:()=>host.name,get:()=>host,session:async()=>({})},
      requestFor:()=>async(path,body)=>{requests.push(body);const corpus=JSON.parse(readFileSync(process.env.RECLAIM_CTL_RESULTS || '.tmp/ctl-results.json'));return {success:true,data:corpus.find(row=>row.method===body.method).result};}};
    await runCtl(['list','--host','fixture','--json'],io);
    assert.ok(JSON.stringify(output).includes('ctl.cleanup.apply'));
    assert.equal(listView('fixture',[d])[0].commands.length,43);
    for (const id of Object.keys(effects)) {
      const op=d.payload.commands['ctl.'+id];
      if (op.effect==='read') continue;
      // --input is stdin only in this in-process harness via a temporary input file.
      const {mkdtempSync,writeFileSync,rmSync}=await import('node:fs');
      const {tmpdir}=await import('node:os'); const {join}=await import('node:path');
      const dir=mkdtempSync(join(tmpdir(),'reclaim-ctl-'));
      try {
        const file=join(dir,'params.json');writeFileSync(file,JSON.stringify(op.examples[0]));
        requests=[];output=[];
        const args=['reclaim',...op.cli,'--host','fixture','--input',file];
        await runCtl([...args,'--dry-run'],io);
        assert.equal(requests.length,0,id+': dry-run dispatched');
        assert.ok(JSON.stringify(output).includes(op.effect));
        output=[];await runCtl(args,io);
        assert.equal(requests.length,0,id+': missing confirmation dispatched');
        assert.ok(JSON.stringify(output).includes('CTL_CONFIRM_REQUIRED'));
        process.exitCode=0;output=[];await runCtl([...args,'--yes','--no-wait'],io);
        assert.equal(requests.length,1,id);
        assert.equal(process.exitCode || 0,0,JSON.stringify(output));
        assert.equal(requests[0].method,op.flow.start || op.flow.method);
        assert.deepEqual(requests[0].params,op.examples[0]);
      } finally {rmSync(dir,{recursive:true,force:true});}
    }
  } finally {CtlClient.prototype.discover=original;process.exitCode=0;}
});

test('job result schema retains Deep report and plan categories, DSM tools and manual follow-ups', async () => {
  const {description:d} = await description();
  const schema = d.payload.commands['ctl.job.result'].resultSchema;
  assert.ok(schema);
  const ajv = new Ajv({strict:false});
  const report = JSON.parse(readFileSync('ui/fixtures/rootfs.report/deep-short.json'));
  const plan = JSON.parse(readFileSync('ui/fixtures/rootfs.plan/deep-short.json'));
  assert.equal(ajv.validate(schema, {command:'job.result',schema_version:1,cleanable:report.cleanable}),true,JSON.stringify(ajv.errors));
  const tools = plan.tools.map(({tool, estimated_bytes})=>({tool,...(estimated_bytes === undefined ? {} : {estimated_bytes})}));
  assert.equal(ajv.validate(schema, {command:'job.result',schema_version:1,moves:plan.moves,tools,follow_up:plan.follow_up}),true,JSON.stringify(ajv.errors));
  assert.equal(ajv.validate(schema,{cleanable:{...report.cleanable,deep:{...report.cleanable.deep,tools:['private-tool']}}}),false);
  assert.equal(ajv.validate(schema,{moves:{...plan.moves,by_category:[{category:'private-category',bytes:1,items:1}]}}),false);
  assert.equal(ajv.validate(d.payload.commands['ctl.rootfs.plan'].paramsSchema,{to:'/volume1/example',mode:'deep'}),true);
});


test('diagnostics volume is optional and snapshot results declare space and simulation fields', async () => {
  const {description:d} = await description();
  assert.deepEqual(operationInput(d.payload.commands['ctl.doctor.run'], [], ''), {});
  const schema = d.payload.commands['ctl.snapshots.list'].resultSchema;
  assert.ok(schema);
  const ajv = new Ajv({strict:false});
  const simulation = {selected_ids:[257],not_materialized_ids:[],freed_bytes:10,unknown_bytes:2,upper_bound_bytes:12,exact:false,inexact_reasons:['unknown_owners']};
  const snapshot = {subvolume_id:257,exclusive_bytes:10,shared_with_live_bytes:20,unknown_bytes:2,qgroup_exclusive_bytes:null,pinned_unreferenced_bytes:null,materialized:true,read_only:true,pending_deletion:false};
  const value = {command:'snapshots.list',schema_version:1,results:[{shares:[{live_subvolume_id:256,held_only_by_snapshots_bytes:10,unknown_bytes:2,snapshots_not_materialized:0,snapshots:[snapshot],simulation}]}]};
  assert.equal(ajv.validate(schema,value), true, JSON.stringify(ajv.errors));
  assert.equal(ajv.validate(schema,{results:[{shares:[{simulation:{...simulation,freed_bytes:'private'}}]}]}), false);
  assert.equal(ajv.validate(schema,{results:[{shares:[{simulation:{...simulation,inexact_reasons:['private']}}]}]}), false);
});

test('AI drafts invoke only the matching non-mutating preview and a dry-run plan', async () => {
  const {runAiTool} = await import('@synopackageland/cli/dist/ctl/ai-tools.js');
  const {description:d} = await description();
  const listing={apps:[d], installed:[{id:d.payload.package.id,version:d.payload.package.version,aliases:d.payload.aliases}],diagnostics:[],cacheHits:0};
  const requests=[];
  const client=new CtlClient({session:async()=>({})},{name:'fixture'},async(_path,body)=>{
    requests.push(body);
    const target=Object.entries(d.payload.commands).find(([,op])=>op.preview===body.method);
    assert.ok(target, 'AI dispatched a mutation');
    return {success:true,data:{operation:target[0],effect:target[1].effect,confirmation_required:true,
      authorization:{command:'authorization.status',schema_version:1,state:'ready'},selection:{},limitations:'Submission preview only'}};
  });
  client.discover=async()=>listing;
  for(const [name,op] of Object.entries(d.payload.commands)) {
    if(op.ai!=='draft')continue;
    requests.length=0;
    const result=await runAiTool(client,'reclaim',name,op.examples[0]);
    assert.equal(result.confirmationRequired,true);
    assert.equal(result.draft.dryRun,true);
    assert.equal(requests.length,1);
    assert.equal(requests[0].method,op.preview);
    assert.deepEqual(requests[0].params,op.examples[0]);
  }
});

test('job flows validate admission, completion and partial results without inventing completion', async () => {
  const {description:d}=await description();
  const listing={apps:[d],installed:[{id:d.payload.package.id,version:d.payload.package.version,aliases:d.payload.aliases}],diagnostics:[],cacheHits:0};
  for(const [name,op] of Object.entries(d.payload.commands)) {
    if(op.flow.type!=='job')continue;
    for(const wait of [false,true]) {
      const requests=[];
      const client=new CtlClient({session:async()=>({})},{name:'fixture'},async(_path,body)=>{
        requests.push(body);
        if(body.method===name)return {success:true,data:{accepted:true,job_id:'j-example'}};
        assert.deepEqual(body.params,{job_id:'j-example'});
        if(body.method===op.flow.poll)return {success:true,data:{job_id:'j-example',state:'partial'}};
        assert.equal(body.method,op.flow.result);
        return {success:true,data:{command:'job.result',schema_version:1,completion:'partial',exit:3}};
      });
      client.discover=async()=>listing;
      const result=await client.execute('reclaim',op.cli,['--input','-'],{yes:true,wait,stdin:JSON.stringify(op.examples[0])});
      if(wait) {
        assert.equal(result.completion,'partial');
        assert.equal(result.exit,3);
        assert.equal(requests.length,3);
      } else {
        assert.equal(result.job.accepted,true);
        assert.equal(result.job.job_id,'j-example');
        assert.equal(result.completion,undefined);
        assert.equal(requests.length,1);
      }
    }
  }
});

test('the shipped descriptor fits the SDK discovery file bound',()=>{
  // The SPK carries the description as compact JSON (synopkgland build writes
  // JSON.stringify of it); the repository copy is indented for review.
  const shipped=JSON.stringify(JSON.parse(readFileSync('generated/synopkgland-api.json','utf8')))+'\n';
  assert.ok(Buffer.byteLength(shipped)<=1048576);
});

test('a live scan keeps its original job ID past an hour without an App wait budget', async t => {
  const {runFlow} = await import('@synopackageland/cli/dist/ctl/flows.js');
  const {description:d} = await description();
  let elapsed = 0;
  let polls = 0;
  const calls = [];
  t.mock.method(performance, 'now', () => elapsed);
  t.mock.method(globalThis, 'setTimeout', callback => { queueMicrotask(callback); return 0; });
  const result = await runFlow(d.payload.commands['scans.start'].flow, {}, async (method, params) => {
    calls.push({method, params});
    if (method === 'scans.start') return {accepted:true,job_id:'ongoing-scan'};
    assert.deepEqual(params,{job_id:'ongoing-scan'});
    if (method === 'ctl.job.status') {
      elapsed += 3_600_001;
      return {job_id:'ongoing-scan',state:++polls === 1 ? 'running' : 'completed'};
    }
    assert.equal(method,'ctl.job.result');
    return {completion:'complete'};
  }, {wait:true});
  assert.deepEqual(result,{completion:'complete'});
  assert.equal(calls.filter(call=>call.method==='scans.start').length,1);
  assert.equal(polls,2);
});
