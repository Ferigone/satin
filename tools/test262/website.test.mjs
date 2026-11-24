import assert from 'node:assert/strict';
import test from 'node:test';
import {
  completeCounters, count, countersConsistent, findChapter, nativeWebsiteActions, parseOptions, satinFailure, satinFirstDiagnostic, satinSnapshot,
} from './website.mjs';

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

const fatalLoad = {
  documentLoaded: false, summary: null, dom: null,
  process: { code: 1, signal: null, spawnError: null, timedOut: false },
  stderr: 'Satin: ExecutionLimit: Native work budget exhausted\n',
};

test('surfaces a fatal Satin VM budget error when the load exits without a JSON summary', () => {
  assert.equal(satinFirstDiagnostic(fatalLoad), 'ExecutionLimit: Native work budget exhausted');
  assert.equal(satinFirstDiagnostic({ ...fatalLoad, stderr: 'Satin: HeapLimit: Allocation limit exceeded\r\n' }),
    'HeapLimit: Allocation limit exceeded');
  assert.equal(satinFailure(fatalLoad, null, null), 'document-load');
});

test('uses a present JSON summary rather than treating stderr as a JavaScript diagnostic', () => {
  const summary = diagnostics => ({ javascript: { diagnostics } });
  assert.equal(satinFirstDiagnostic({ ...fatalLoad, summary: summary(['reported first', 'reported second']) }),
    'reported first');
  assert.equal(satinFirstDiagnostic({ ...fatalLoad, summary: summary([]) }), null);
});

test('does not turn network, tool, or abnormal process failures into JavaScript diagnostics', () => {
  for (const stderr of [
    'Satin: TLS certificate validation failed\n',
    'Satin: Click target #run does not exist\n',
    'Satin: Error: Connection timed out\nSatin: ExecutionLimit: Nested error text\n',
    'ExecutionLimit: Not a Satin CLI diagnostic\n',
  ]) assert.equal(satinFirstDiagnostic({ ...fatalLoad, stderr }), null);

  for (const process of [
    { code: 0 }, { code: null, signal: 'SIGTERM' }, { code: 1, signal: 'SIGTERM' },
    { code: 1, timedOut: true }, { code: 1, spawnError: 'ENOENT' },
  ]) assert.equal(satinFirstDiagnostic({ ...fatalLoad, process }), null);
});

test('selects an exact loaded chapter label and its advertised plan, never a partial or ambiguous label', () => {
  const label = 'Chapter - harness (55 tests)';
  const labels = ['Loading test file: json/language.json', 'Chapter - annexB (81 tests)', label];
  assert.deepEqual(findChapter(labels, 'harness'), { kind: 'chapter', name: 'harness', label, planned: 55 });
  for (const name of ['Harness', 'harn', 'language']) assert.equal(findChapter(labels, name), null);
  assert.throws(() => findChapter([label, 'Chapter - harness (56 tests)'], 'harness'), /Ambiguous chapter/);
});

test('requires all selected tests, not a self-consistent partial or a different completed scope', () => {
  const partial = { planned: '55', ran: '54', pass: '49', fail: '5', failedToLoad: '0' };
  assert.ok(countersConsistent(partial));
  assert.ok(!completeCounters(partial, 55));
  const complete = { ...partial, ran: '55', pass: '50' };
  assert.ok(completeCounters(complete, 55));
  assert.ok(!completeCounters(complete, 16436));
});

test('Satin chapter names and catalog paths select the same hit-tested chapter action', () => {
  for (const chapter of ['language', 'json/language', 'json/language.json']) {
    const options = parseOptions(['--satin', 'satin', '--chapter', chapter, '--timeout-ms', '120000']);
    assert.equal(options.native, true);
    assert.equal(options.until, 'complete');
    const steps = nativeWebsiteActions(options);
    assert.deepEqual(steps.map(item => item.action), ['title', 'click', 'checkpoint', 'click_in_row', 'checkpoint', 'click', 'checkpoint']);
    assert.deepEqual(steps[3], { action: 'click_in_row', within_id: 'chapterSelector', label_class: 'chapterName',
      label_prefix: 'Chapter - language (', control_alt: 'Run', timeout_ms: 120000 });
    assert.equal(steps[4].text, 'Testing complete!');
    assert.equal(steps[4].timeout_ms, 120000);
    assert.deepEqual(steps.filter(item => item.action === 'checkpoint').map(item => item.name), ['catalog', 'run', 'results']);
  }
  assert.throws(() => parseOptions(['--satin', 'satin', '--chapter', 'json/language/nested']), /chapter name/);
});

test('native Run All records the initial catalog, and first mode pauses after a positive counter', () => {
  const args = ['--satin', 'satin', '--until', 'first', '--site-url', 'http://127.0.0.1:12345/default.html'];
  const options = parseOptions(args);
  assert.equal(options.native, true);
  assert.equal(options.siteUrl, args.at(-1));
  const steps = nativeWebsiteActions(options);
  assert.deepEqual(steps.filter(item => item.action === 'click').map(item => item.id), ['run', 'btnRunAll', 'btnPause', 'results']);
  assert.equal(steps[2].within_id, 'testsToRun');
  assert.equal(steps[4].within_id, 'totalCounter');
  assert.equal(steps[4].positive, true);
  assert.ok(!steps.some(item => item.action === 'click_in_row'));
  assert.equal(parseOptions(['--satin', 'satin']).native, false);
});

test('long full-suite budgets apply to execution while startup and Results waits stay bounded', () => {
  const options = parseOptions(['--satin', 'satin', '--chapter', 'built-ins', '--timeout-ms', '21600000']);
  const steps = nativeWebsiteActions(options);
  assert.equal(steps.find(s => s.name === 'catalog').timeout_ms, 120000);
  assert.equal(steps.find(s => s.action === 'click_in_row').timeout_ms, 120000);
  assert.equal(steps.find(s => s.name === 'run').timeout_ms, 21600000);
  assert.equal(steps.find(s => s.name === 'results').timeout_ms, 15000);
  assert.throws(() => parseOptions(['--satin', 'satin', '--timeout-ms', '21600001']), /Invalid arguments/);
});
