import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { once } from 'node:events';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { resolve } from 'node:path';
import { promisify } from 'node:util';

const [binaryArg = 'target/release/rust-browser.exe', outputArg = 'target/structured-clone-verification',
  mode = 'both', expectedSha, suite = 'core'] = process.argv.slice(2);
assert.ok(['both', 'chrome', 'rust'].includes(mode));
assert.ok(['core', 'platform'].includes(suite));
const binary = resolve(binaryArg);
const output = resolve(outputArg);
const rea = process.env.REA_CLI ??
  'C:/Users/admin/AppData/Local/npm-cache/_npx/da7caeae9db9c589/node_modules/rea-agents/scripts/rea.mjs';
const chrome = process.env.CHROME_EXE ?? 'C:/Program Files/Google/Chrome/Application/chrome.exe';
const fixtureName = suite === 'platform' ? 'structured-clone-platform.html' : 'structured-clone.html';
const fixture = await readFile(new URL('./fixtures/' + fixtureName, import.meta.url));
const run = promisify(execFile);
const sha = bytes => createHash('sha256').update(bytes).digest('hex');
const browserSha256 = mode === 'chrome' ? null : sha(await readFile(binary));
if (expectedSha) assert.equal(browserSha256, expectedSha.toLowerCase());
await mkdir(output, { recursive: true });
const save = (name, value) => writeFile(resolve(output, name), JSON.stringify(value, null, 2) + '\n');
const report = {
  fixtureSha256: sha(fixture), runnerSha256: sha(await readFile(new URL(import.meta.url))),
  browserSha256, runs: {},
};
const options = { windowsHide: true, maxBuffer: 4 * 1024 * 1024 };
const server = createServer((request, response) => {
  if (request.url !== '/clone') return response.writeHead(404).end();
  response.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8', 'Cache-Control': 'no-store' });
  response.end(fixture);
});
server.listen(0, '127.0.0.1');
await once(server, 'listening');
const url = 'http://127.0.0.1:' + server.address().port + '/clone';

function receipt(dom) {
  const match = /<pre\b(?=[^>]*\bid="receipt")(?=[^>]*\bdata-complete="yes")[^>]*>([^<]*)<\/pre>/.exec(dom);
  assert.ok(match, 'Missing structured clone receipt');
  const entities = { amp: '&', lt: '<', gt: '>', quot: '"', '#39': "'" };
  return JSON.parse(match[1].replace(/&(amp|lt|gt|quot|#39);/g, (_, key) => entities[key]));
}

const expectedRust = suite === 'platform' ? {
  files: [true, true, true, 2, 'application/x-clone', '5,6', 'żółć.txt', 42, 'text/plain', 'foo!', true],
  exception: [true, true, 'AbortError', 20, 'przerwano połączenie', true],
  exceptionUtf16: 55296,
  exceptionIntrinsics: ['AbortError', 'AbortError', 0],
  rollback: ['DataCloneError', 12, 3],
  image: [true, 1, 1, 'srgb', 'rgba-unorm8', '3,9,15,255'],
  imageGraph: [true, true, 4, 44],
  transfer: [0, '3,9,15,255', true, true, 4],
  float16: [true, 'display-p3', 'rgba-float16', '0.125,0.5,0.875,1', 0],
} : {
  graph: [true, true, true, 4, false, true, false, true, 9, true, true, 55296],
  getters: ['first,child', true, 2, false, false, true],
  intrinsics: [923, true, 'abc', 'gi', 0, true, true, true, true, 57343, true, 'message', true],
  binary: [5, '1,2,3,4,5', true, 12, true, true, true, 1, 7, 2, 2, '11,3,4,5,0,0,0'],
  transfer: [true, true, true, 12, '2,17,4,5', 6, 'options,iterator,getter'],
  errors: [...Array(8).fill('DataCloneError'), false, 'DataCloneError', false, true],
  binding: [1, true, 1, 'TypeError', 'TypeError', 'TypeError', 'TypeError'],
};
const platformDifferences = ['exceptionUtf16', 'imageGraph', 'transfer'];
const expectedChrome = suite === 'platform' ? {
  ...expectedRust,
  exceptionUtf16: 'null',
  imageGraph: [false, false, 0, 3],
  transfer: [0, '3,9,15,255', false, false, 4],
} : expectedRust;

try {
  if (mode !== 'chrome') {
    const domPath = resolve(output, 'rust-dom.html');
    const result = await run(binary, ['--headless', '--url', url,
      '--profile', resolve(output, 'profile-' + randomUUID()), '--run-for-ms', '600',
      '--dump-dom', domPath], { ...options, timeout: 20000 });
    const processReport = JSON.parse(result.stdout);
    assert.deepEqual(processReport.javascript.diagnostics, []);
    const values = receipt(await readFile(domPath, 'utf8'));
    report.runs.rust = { diagnostics: processReport.javascript.diagnostics, values };
    assert.deepEqual(values, expectedRust);
    assert.equal(sha(await readFile(binary)), browserSha256);
  }
  if (mode !== 'rust') {
    const scenario = {
      browser: { mode: 'launch', executable_path: chrome, headless: true },
      start_url: { url },
      actions: [{
        step_id: 'receipt', action: 'wait_for', state: 'attached', timeout_ms: 10000,
        locator: { kind: 'css', selector: '#receipt[data-complete="yes"]' },
      }],
      capture: { at_end: ['url', 'dom'], events: ['page-errors'] },
      command_timeout_ms: 25000,
    };
    await save('chrome-scenario.json', scenario);
    const result = await run(process.execPath, [rea, 'capture-browser-scenario',
      resolve(output, 'chrome-scenario.json'), '--format', 'json', '--full-output'],
    { ...options, timeout: 40000 });
    const captured = JSON.parse(result.stdout);
    await save('chrome-rea.json', captured);
    const normalized = captured.data.normalized_result;
    const step = normalized.steps.at(-1);
    assert.ok(captured.ok);
    assert.equal(step.status, 'completed');
    assert.equal(normalized.browser.process_ownership, 'provider-owned');
    assert.equal(normalized.browser.cleanup, 'terminated-owned-process');
    report.runs.chrome = {
      evidenceId: captured.data.evidence_id,
      browser: normalized.browser,
      values: receipt(step.artifacts.dom.value.text),
    };
  }
  if (mode === 'both') {
    report.matches = Object.keys(expectedRust).filter(key =>
      JSON.stringify(report.runs.rust.values[key]) === JSON.stringify(report.runs.chrome.values[key]));
    report.differences = Object.keys(expectedRust).filter(key => !report.matches.includes(key));
  }
  await save('report.json', report);
  if (mode !== 'rust') assert.deepEqual(report.runs.chrome.values, expectedChrome);
  if (mode === 'both') assert.deepEqual(report.differences,
    suite === 'platform' ? platformDifferences : [], 'Unexpected structured-clone browser differences');
  console.log(JSON.stringify(report));
} finally {
  await new Promise(accept => server.close(accept));
}
