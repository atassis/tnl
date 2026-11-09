import {test} from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import * as validators from '../frontend/inspector/src/api/generated/validators.js';
const directory = new URL('../frontend/inspector/src/api/generated/', import.meta.url);
const fixtures = JSON.parse(await readFile(new URL('fixtures.json',directory)));
const schemas = JSON.parse(await readFile(new URL('schemas.json',directory)));
test('all Rust DTO fixtures validate and unknown versions are rejected', () => {
  for (const [name, fixture] of Object.entries(fixtures)) {
    assert(validators[`validate_${name}`](fixture), name);
    assert(!validators[`validate_${name}`]({...fixture,schema_version:2}), name);
  }
});
test('IDs, nullability, enums and strict mutation keys are checked at runtime', () => {
  assert(!validators.validate_confirm_request({...fixtures.confirm_request,replay_id:'invalid'}));
  assert(!validators.validate_confirm_request({...fixtures.confirm_request,extra:true}));
  assert(!validators.validate_prepare_request({...fixtures.prepare_request,extra:true}));
  assert(!validators.validate_detail({...fixtures.detail,state:'fictional'}));
  const detail = structuredClone(fixtures.detail);
  delete detail.request.body.text;
  assert(!validators.validate_detail(detail));
  assert(!validators.validate_stats({...fixtures.stats,records:-1}));
});
test('generated validation uses no runtime schema compilation or authority data', async () => {
  const code = await readFile(new URL('validators.js',directory),'utf8');
  assert(!/\bnew Function\b|\beval\s*\(/.test(code));
  assert(!JSON.stringify(schemas).includes('PRIVATE_TICKET_CANARY'));
  assert(!schemas.detail.properties.started);
  assert(!schemas.detail.properties.counters);
});
test('timing nested version and JS-safe nonnegative integer bounds are enforced', () => {
  assert.equal(fixtures.detail.timing.availability, 'available');
  assert.equal(fixtures.detail.timing.snapshot.backend_first_byte_offset_us, 5);
  for (const value of [-1, 0.5, 9_007_199_254_740_992, Infinity, NaN]) {
    const detail = structuredClone(fixtures.detail);
    detail.timing.snapshot.as_of_offset_us = value;
    assert(!validators.validate_detail(detail), String(value));
  }
  const detail = structuredClone(fixtures.detail);
  detail.timing.schema_version = 2;
  assert(!validators.validate_detail(detail));
});
