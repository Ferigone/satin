import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { compareReports } from './delta.mjs';

const fixturePath = name => fileURLToPath(new URL(`./fixtures/delta-${name}.json`, import.meta.url));
const baseline = JSON.parse(readFileSync(fixturePath('baseline'), 'utf8'));
const candidate = JSON.parse(readFileSync(fixturePath('candidate'), 'utf8'));
const cli = (...args) => spawnSync(process.execPath, [
  fileURLToPath(new URL('./delta.mjs', import.meta.url)), ...args,
], { encoding: 'utf8', windowsHide: true });

test('equivalent reports ignore row order, repeated/reordered paths and diagnostic edits', () => {
  const equivalent = structuredClone(baseline);
  equivalent.selection.paths.reverse().push(equivalent.selection.paths[0]);
  equivalent.results.reverse().find(row => row.variant === null).reason = 'another diagnostic';

  const delta = compareReports(baseline, equivalent);
  assert.deepEqual(delta.counts, {
    unchanged: 8, lostCoverage: 0, regressions: 0, improvements: 0, nonPassingChanges: 0,
    new: 0, missing: 0, excluded: 0,
  });
  assert.deepEqual(delta.scope, {
    test262Revision: baseline.test262Revision,
    paths: ['built-ins/Array', 'language/delta'],
    offset: 0,
    limit: null,
  });
});

test('status changes are keyed by path and variant, with coverage changes separate', () => {
  const delta = compareReports(baseline, candidate);
  const entry = (name, variant, baselineStatus, candidateStatus) => ({
    path: `test/language/delta/${name}.js`, variant, baselineStatus, candidateStatus,
  });
  assert.deepEqual(delta.changes, {
    regressions: [entry('dual', 'strict', 'pass', 'fail')],
    improvements: [entry('repaired', 'raw', 'fail', 'pass')],
    nonPassingChanges: [entry('unsupported', 'module', 'unsupported', 'resource_limit')],
    new: [entry('new', 'module', null, 'pass')],
    missing: [entry('absent', 'strict', 'fail', null)],
    excluded: [
      entry('excluded', 'sloppy', 'pass', 'excluded'),
      entry('restored', 'raw', 'excluded', 'pass'),
    ],
  });
  assert.deepEqual(delta.counts, {
    unchanged: 2, lostCoverage: 2, regressions: 1, improvements: 1, nonPassingChanges: 1,
    new: 1, missing: 1, excluded: 2,
  });
  assert.deepEqual(delta.workerSha256, { baseline: baseline.workerSha256, candidate: candidate.workerSha256 });
  assert.deepEqual(compareReports(
    { ...baseline, results: baseline.results.toReversed() },
    { ...candidate, results: candidate.results.toReversed() },
  ), delta);
});

test('revision, path, offset and limit mismatches are named in the rejection', () => {
  assert.throws(() => compareReports(baseline, {
    ...candidate, test262Revision: '0'.repeat(40),
  }), /scope mismatch:\ntest262Revision: baseline .*candidate "000/);
  for (const [key, value] of [['paths', ['different']], ['offset', 1], ['limit', 1]]) {
    assert.throws(() => compareReports(baseline, {
      ...candidate, selection: { ...candidate.selection, [key]: value },
    }), new RegExp(`scope mismatch:\\n${key}:`));
  }
});

test('malformed reports and duplicate keys are rejected rather than silently merged', () => {
  assert.throws(() => compareReports({ ...baseline, schema: 2 }, candidate), /baseline: expected report schema 1/);
  assert.throws(() => compareReports(baseline, { ...candidate, selection: {} }), /candidate: invalid selection/);
  assert.throws(() => compareReports(baseline, {
    ...candidate, results: [...candidate.results, candidate.results[0]],
  }), /candidate: duplicate result.*restored\.js.*raw/);
  assert.throws(() => compareReports(baseline, {
    ...candidate, results: [{ ...candidate.results[0], status: 'skipped' }],
  }), /candidate: invalid result at index 0/);
});

test('CLI emits JSON for comparisons and diagnostics with no JSON for scope errors', () => {
  const same = cli(fixturePath('baseline'), fixturePath('baseline'));
  assert.equal(same.status, 0, same.stderr);
  assert.equal(JSON.parse(same.stdout).counts.unchanged, 8);

  const changed = cli(fixturePath('baseline'), fixturePath('candidate'));
  assert.equal(changed.status, 1, changed.stderr);
  assert.deepEqual(JSON.parse(changed.stdout), compareReports(baseline, candidate));

  const mismatch = cli(fixturePath('baseline'), fixturePath('other-scope'));
  assert.equal(mismatch.status, 2);
  assert.equal(mismatch.stdout, '');
  assert.match(mismatch.stderr, /Test262 scope mismatch:\noffset: baseline 0, candidate 1\nlimit: baseline null, candidate 2/);
});

test('CLI help documents exit codes; malformed JSON fails with a diagnostic', () => {
  const help = cli('--help');
  assert.equal(help.status, 0, help.stderr);
  assert.match(help.stdout, /Usage:.* BASELINE\.json CANDIDATE\.json/);
  assert.match(help.stdout, /Exit codes: 0 =.*1 =[\s\S]+2 =/);

  const badJson = cli(fixturePath('baseline'), fileURLToPath(import.meta.url));
  assert.equal(badJson.status, 2);
  assert.equal(badJson.stdout, '');
  assert.match(badJson.stderr, /^candidate \(.+delta\.test\.mjs\):/);
});
