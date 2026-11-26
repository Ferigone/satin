import assert from 'node:assert/strict';
import test from 'node:test';
import { budgetTrace, executionProfiles, safeDiagnostic } from './public-media-diagnostics.mjs';

test('CPU profiles accept fixed counters and discard arbitrary data and malformed buckets', () => {
  const data = { node: 1, bytes: 20, sha256: 'a'.repeat(64), external: false,
    error: 'ExecutionLimit', wallUs: 450, cpuUs: 0, work: [1,2,3,4,5,6],
    cycles: null, secret: 'do not export' };
  const line = value => 'SATIN_QA_SCRIPT ' + JSON.stringify(value);
  const profiles = executionProfiles([line(data), line({ ...data, work: [1] }),
    line({ ...data, sha256: ['a'.repeat(64)] }), line({ ...data, error: data.secret })].join('\n'));
  assert.equal(profiles.length, 1);
  assert.equal(profiles[0].work.propertyRead, 5);
  assert.equal(profiles[0].cpuUs, 0);
  assert.equal(profiles[0].cycles, null);
  assert.ok(!JSON.stringify(profiles).includes(data.secret));
});

test('preserves known startup and memory budget classes', () => {
  assert.deepEqual(safeDiagnostic('Dynamic import: ExecutionLimit: Nested bytecode depth budget exhausted'), {
    kind: 'ExecutionLimit', cause: 'Nested bytecode depth budget exhausted', inline: null,
    phase: 'dynamic-import', source: null, asset: null,
  });
  assert.deepEqual(safeDiagnostic('inline script #293: HeapLimit: Heap cell budget exhausted'), {
    kind: 'HeapLimit', cause: 'Heap cell budget exhausted', inline: '293',
    phase: 'inline-script', source: null, asset: null,
  });
});

test('neither arbitrary hostnames nor request and error contents are retained', () => {
  const normalized = safeDiagnostic('https://test.user.example/private?secret=do-not-retain: denied');
  assert.deepEqual(normalized, {
    kind: 'Other', cause: null, inline: null, phase: null, source: 'other', asset: null,
  });
  assert.equal(safeDiagnostic('https://assets.twitch.tv.example/file: Error').source, 'other');
  assert.equal(safeDiagnostic('https://assets.twitch.tv/secret?path=foo: HeapLimit').source, 'twitch-cdn');
  assert.equal(safeDiagnostic('https://assets.twitch.tv/assets/core-hash.js: HeapLimit').asset, 'twitch-core');
  assert.equal(safeDiagnostic('https://www.youtube.com/s/player/secret/base.js?private=secret: HeapLimit').asset, 'youtube-player');
});

test('cost breakdowns belong to their failed work task', () => {
  const trace = budgetTrace([
    'SATIN_QA_COST 99 2 unpaired.rs',
    'SATIN_QA_BUDGET native 8 3 20 11 crates/javascript/src/vm.rs',
    'SATIN_QA_COST 14 5 C:\\Private\\crates\\javascript\\src\\lexer.rs',
    'SATIN_QA_COST 3 8 C:\\Private\\hidden.rs',
    'SATIN_QA_BUDGET compiler 5 1 0 2 crates/javascript/src/lexer.rs',
    'SATIN_QA_COMPILER 500 100 100 90 3',
    'SATIN_QA_COST 99 2 dropped.rs',
  ].join('\n'));
  assert.deepEqual(trace[0].topWork, [
    { units: 14, line: 5, file: 'crates/javascript/src/lexer.rs' },
    { units: 3, line: 8, file: null },
  ]);
  assert.equal(trace[1].topWork, undefined);
  assert.deepEqual(trace[1].compilation, { initialBytes: 500, sourceUnits: 100, backingBytes: 100, offset: 90, compactedBodies: 3 });
});

test('budget traces only expose source paths inside the two engine crates', () => {
  assert.deepEqual(budgetTrace('discard\nSATIN_QA_BUDGET native 1234 5 741 95 C:\\User\\private\\crates\\engine\\src\\script.rs\n' +
    'SATIN_QA_BUDGET instruction 1 0 0 0 -\nSATIN_QA_BUDGET heap 2 1 31 9 C:\\private\\token.txt\n' +
    'SATIN_QA_BUDGET other 5 1 8 9 whatever'), [
    { kind: 'native', requested: 1234, remaining: 5, nativeWork: 741, line: 95, file: 'crates/engine/src/script.rs' },
    { kind: 'instruction', requested: 1, remaining: 0, nativeWork: 0, line: 0, file: null },
    { kind: 'heap', requested: 2, remaining: 1, nativeWork: 31, line: 9, file: null },
  ]);
});
