import assert from 'node:assert/strict';
import test from 'node:test';
import { reportNativeHarnessArtifacts as report, verifyNativeHarnessArtifacts as verify } from './native-checkpoint-verify.mjs';
import { site } from './website.mjs';

const runId = '34-123456789';
const title = 'ECMAScript Language – test262';
const png = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/p9kAAAAASUVORK5CYII=', 'base64');
const suite = '<span class="targetTestSuiteVersion">1234</span><span class="targetTestSuiteDate">2016-03-23</span>';
const nav = selected => `<a class="nav-link${selected === 'run' ? ' selected' : ''}" id="run" href="#">Run</a>
<a id="results" href="#" class="nav-link${selected === 'results' ? ' selected' : ''}">Results</a>`;
const runHtml = `${nav('run')}<div class="content-tests">
<div id="progressbar"><div class="progress"><span class="text">Testing complete!</span></div></div>
<div id="chapterSelector"><table><tr><td class="chapterName">Chapter - harness (55 tests)</td></tr>
<tr><td class="chapterName">Chapter - annexB (81 tests)</td></tr></table></div>
<span id="testsToRun">55</span><span id="totalCounter">55</span><span id="Pass">53</span><span id="Fail">2</span>
<span id="failedToLoadCounter1">1</span>${suite}</div>`;
const resultsHtml = `${nav('results')}<div class="content-results" style="display: block;">
<span class="totalCases">55</span><span class="passedCases">53</span><span class="failedCases">2</span>
<span id="failedToLoadCounter">1</span>${suite}</div>`;

function sample() {
  const identity = { tabId: 1, documentNavigation: 4, runId, title };
  return {
    manifest: {
      schema: 1, scope: 'legacy-test262-native-chapter', url: site,
      selection: { kind: 'chapter', name: 'harness', planned: 55 },
      suite: { version: '1234', date: '2016-03-23' },
      files: { process: 'process.json', stdout: 'frame.stdout', frame: 'frame.png' },
    },
    process: { code: 0, signal: null, timedOut: false, spawnError: null },
    summary: {
      window_smoke: true, frames_presented: 2, layout_limited: false, paint_limited: false,
      url: `${site}#`, title, checkpoints: { runId, names: ['run', 'results'] },
      javascript: { diagnostics: ['inline script #9: Thrown: <thrown object>'] },
    },
    run: { ...identity, url: site, html: runHtml },
    results: { ...identity, url: `${site}#`, html: resultsHtml },
    png,
  };
}

test('validates same-attempt harness artifacts without claiming a visual result or requiring all cases pass', () => {
  assert.deepEqual(verify(sample()), {
    status: 'artifacts-consistent', visualConfirmationRequired: true,
    selection: { kind: 'chapter', name: 'harness', planned: 55 },
    suite: { version: '1234', date: '2016-03-23' },
    counters: { planned: 55, ran: 55, pass: 53, fail: 2, failedToLoad: 1 },
  });
});

test('rejects leftover snapshots after error, a missing success envelope, or invalid frame artifacts', () => {
  for (const process of [{ code: 1 }, { code: 0, timedOut: true }, { code: 0, signal: 'SIGTERM' }, { code: 0, spawnError: 'ENOENT' }]) {
    assert.throws(() => verify({ ...sample(), process }), /process did not succeed/);
  }
  const invalid = sample();
  invalid.summary.checkpoints.names.reverse();
  assert.throws(() => verify(invalid), /ordered checkpoint success envelope/);
  delete invalid.summary.checkpoints;
  assert.throws(() => verify(invalid), /ordered checkpoint success envelope/);
  assert.throws(() => verify({ ...sample(), png: png.subarray(0, 33) }), /invalid final PNG/);
});

test('rejects mixed attempts even with matching tab and navigation IDs, and same-URL reloads', () => {
  const mixed = sample();
  mixed.results.runId = '35-123456789';
  assert.throws(() => verify(mixed), /stale or from another attempt/);
  for (const [field, value] of [['tabId', 2], ['documentNavigation', 5]]) {
    const changed = sample();
    changed.results[field] = value;
    assert.throws(() => verify(changed), /Tab or document changed/);
  }
  const otherSite = sample();
  otherSite.results.url = `${site}?unverified=yes`;
  assert.throws(() => verify(otherSite), /invalid checkpoint document/);
});

test('rejects hidden, missing, or ambiguous Results rather than treating counters as zero', () => {
  for (const html of [
    resultsHtml.replace('display: block;', 'display: none;'),
    resultsHtml.replace(nav('results'), nav('run')),
    resultsHtml.replace('<span id="failedToLoadCounter">1</span>', ''),
    resultsHtml.replace('<span class="totalCases">55</span>', '<span class="totalCases">55</span><span class="totalCases">55</span>'),
    resultsHtml.replace('<span class="totalCases">55</span>', '<template><span class="totalCases">55</span></template>'),
    resultsHtml.replace('<span class="totalCases">55</span>', '<script>var fake = \'<span class="totalCases">55</span>\';</script>'),
  ]) {
    const input = sample();
    input.results.html = html;
    assert.throws(() => verify(input), /pane not selected|Missing or ambiguous/);
  }
  const blank = sample();
  blank.results.html = blank.results.html.replace('id="failedToLoadCounter">1', 'id="failedToLoadCounter">');
  assert.throws(() => verify(blank), /Results counters do not match/);
});

test('rejects a partial run, wrong chapter, changed suite, and mismatched Results', () => {
  for (const [stage, from, to, error] of [
    ['run', 'Testing complete!', '99 %', /completion marker absent/],
    ['run', 'id="totalCounter">55', 'id="totalCounter">54', /all 55 harness tests/],
    ['run', 'harness (55 tests)', 'other (55 tests)', /Wrong or ambiguous chapter/],
    ['run', 'annexB (81 tests)', 'other (55 tests)', /Wrong or ambiguous chapter/],
    ['run', 'id="failedToLoadCounter1">1', 'id="failedToLoadCounter1">3', /all 55 harness tests/],
    ['results', 'targetTestSuiteVersion">1234', 'targetTestSuiteVersion">1235', /Suite identity/],
    ['results', 'class="passedCases">53', 'class="passedCases">55', /Results counters do not match/],
  ]) {
    const input = sample();
    input[stage].html = input[stage].html.replace(from, to);
    assert.throws(() => verify(input), error);
  }
});

test('requires the explicit harness manifest and its suite identity, with local artifact names', () => {
  assert.throws(() => verify({ ...sample(), manifest: null }), /manifest scope/);
  const wrongScope = sample();
  wrongScope.manifest.selection.name = 'built-ins';
  assert.throws(() => verify(wrongScope), /55-case harness/);
  const changedSuite = sample();
  changedSuite.manifest.suite.version = 'new';
  assert.throws(() => verify(changedSuite), /Suite identity/);
  const externalFile = sample();
  externalFile.manifest.files.frame = '../frame.png';
  assert.throws(() => verify(externalFile), /simple filenames/);
});

test('reports observed zero, blank, nonexistent element and missing/invalid snapshots separately', () => {
  const input = sample();
  input.run.html = input.run.html.replace('id="totalCounter">55', 'id="totalCounter">0')
    .replace('id="Pass">53', 'id="Pass">').replace('<span id="failedToLoadCounter1">1</span>', '');
  input.results = null;
  const output = report(input);
  assert.equal(output.status, 'unverified');
  assert.equal(Object.hasOwn(output, 'counters'), false);
  assert.deepEqual(output.observed.run.ran, { state: 'observed', raw: '0', value: 0 });
  assert.deepEqual(output.observed.run.pass, { state: 'blank', raw: '', value: null });
  assert.deepEqual(output.observed.run.failedToLoad, { state: 'missing-element', raw: null, value: null });
  assert.deepEqual(output.observed.results.ran, { state: 'missing-snapshot', raw: null, value: null });
  input.artifacts = { results: 'invalid' };
  assert.equal(report(input).observed.results.ran.state, 'invalid-snapshot');
});

test('keeps partial and timed-out observations separate from completions', () => {
  const input = sample();
  input.run.html = input.run.html.replace('id="totalCounter">55', 'id="totalCounter">54');
  assert.equal(report(input).status, 'partial');
  assert.equal(report(input).observed.run.ran.value, 54);
  input.process.timedOut = true;
  assert.equal(report(input).status, 'timeout');
  assert.equal(report(input).observed.run.ran.value, 54);
  assert.equal(Object.hasOwn(report(input), 'counters'), false);
});
