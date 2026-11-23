import assert from 'node:assert/strict';
import test from 'node:test';
import { count, countersConsistent, satinFailure, satinSnapshot } from './website.mjs';

const initial = `<a href="#" class="nav-link" id="run">Run</a>
<img src="images/runall.png" alt="Run All" id="btnRunAll">
<span class="teststorun-counter" id="testsToRun"></span>
<span class="total-counter" id="totalCounter"></span>`;

test('preserves absent, blank and explicit zero counters separately', () => {
  const before = satinSnapshot(initial);
  assert.deepEqual(before.counters, { planned: '', ran: '', pass: null, fail: null, failedToLoad: null });
  const after = satinSnapshot(initial.replace('id="totalCounter"></span>', 'id="totalCounter">0</span>'));
  assert.equal(after.counters.ran, '0');
  assert.equal(count(after.counters.ran), 0);
  assert.equal(count(before.counters.ran), null);
  assert.equal(count(before.counters.pass), null);
});

test('failed to load is included in fail, not counted again', () => {
  const counters = { planned: '16436', ran: '4', pass: '2', fail: '2', failedToLoad: '1' };
  assert.ok(countersConsistent(counters));
  assert.ok(!countersConsistent({ ...counters, ran: '5' }));
  assert.ok(!countersConsistent({ ...counters, failedToLoad: '3' }));
  assert.ok(!countersConsistent({ ...counters, failedToLoad: '' }));
  assert.ok(!countersConsistent({ ...counters, planned: '3' }));
});

test('reports the earliest blocked stage even if a later input attempt also failed', () => {
  const native = { attempt: { process: { code: 1 }, stderr: 'Satin: Smoke target #btnRunAll has no rendered box\n' } };
  const load = { documentLoaded: true, dom: satinSnapshot(initial) };
  assert.equal(satinFailure(null, null, native), 'document-load');
  assert.equal(satinFailure(load, load, native), 'site-initialization');
  const initialized = { documentLoaded: true, dom: satinSnapshot(initial.replace('id="run"', 'id="run" targetdiv=".content-tests"')) };
  assert.equal(satinFailure(initialized, initialized, native), 'catalog-load');
  initialized.dom.counters.planned = '16436';
  assert.equal(satinFailure(initialized, initialized, native), 'run-tab');
  const tab = { dom: { runClass: 'selected nav-link' } };
  assert.equal(satinFailure(initialized, tab, native), 'run-all-control');
  assert.equal(satinFailure(initialized, tab, null), null);
});
