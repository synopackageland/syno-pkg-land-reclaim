import {test} from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import Ajv from 'ajv';
import {validateRegistry, checkRouteEquality} from '@synopackageland/cli/dist/ctl/registry.js';

const load = () => JSON.parse(readFileSync('operations.json'));
test('the v2 source covers every old command with schemas and explicit AI policy', () => {
  const registry = load();
  validateRegistry(registry);
  const operations = Object.values(registry.operations);
  assert.equal(operations.filter(op => op.ai === 'draft').length, 11);
  assert.equal(operations.filter(op => op.ai === 'preview').length, 11);
  for (const [name, op] of Object.entries(registry.operations)) {
    assert.equal(op.permission, 'administrators', name);
    assert.ok(Object.keys(op.resultSchema).length, name);
    if (op.ai === 'draft') {
      const preview = registry.operations[op.preview];
      assert.equal(preview.ai, 'preview');
      assert.equal(preview.effect, 'read');
      assert.deepEqual(preview.paramsSchema, op.paramsSchema);
    }
    if (op.longRunning) assert.equal(op.flow.type, 'job');
  }
  const routes = JSON.parse(readFileSync('generated/routes.json'));
  checkRouteEquality(JSON.parse(readFileSync('app/synopkgland-operations.json')), Object.entries(routes).map(([method, op]) => ({method, permission:op.permission})));
  assert.throws(() => checkRouteEquality(registry, []));
  const manifest = JSON.parse(readFileSync('dsm-package.yaml'));
  assert.equal(manifest.ctl.registry, 'operations.json');
  assert.equal(manifest.ctl.commands, undefined);
});

test('schemas validate responses collected through real Rust handlers, including every original command', () => {
  const registry = load();
  const corpus = JSON.parse(readFileSync(process.env.RECLAIM_CTL_RESULTS || '.tmp/ctl-results.json'));
  const ajv = new Ajv({strict:false});
  const covered = new Set();
  for (const {method, result} of corpus) {
    const op = registry.operations[method];
    assert.ok(op, method);
    assert.equal(ajv.validate(op.resultSchema, result), true, method + ': ' + JSON.stringify(ajv.errors));
    covered.add(method);
  }
  for (const [name, op] of Object.entries(registry.operations)) {
    if (op.ai !== 'hidden') assert.ok(covered.has(name), name + ': no real handler result');
  }
  for (const method of ['ctl.volumes.list','ctl.job.result','ctl.snapshots.list']) {
    const schema = registry.operations[method].resultSchema;
    assert.equal(ajv.validate(schema, {command:method.slice(4), schema_version:1, exit:'wrong'}), false);
    assert.equal(ajv.validate(schema, {command:method.slice(4), schema_version:1, password:'private'}), false);
  }
  const accepted = registry.operations['scans.start'].resultSchema;
  assert.equal(ajv.validate(accepted, {accepted:true,job_id:'j-example'}), true);
  assert.equal(ajv.validate(accepted, {accepted:true,job_id:'j-example',completed:true}), false);
});
