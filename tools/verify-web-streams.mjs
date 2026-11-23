import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { once } from 'node:events';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { posix, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const repo = fileURLToPath(new URL('../', import.meta.url));
const [binaryArg, outputArg, mode = 'both', ...filter] = process.argv.slice(2);
assert.ok(['both', 'rust', 'chrome'].includes(mode),
  'Usage: node tools/verify-web-streams.mjs [BROWSER.exe] [OUTPUT] [both|rust|chrome] [fixture|STREAMS_WPT_FILE ...]');
const binary = resolve(binaryArg ?? resolve(repo, 'target/release/rust-browser.exe'));
const output = resolve(outputArg ?? resolve(repo, 'target/web-streams-verification'));
const chrome = process.env.CHROME_EXE ?? 'C:/Program Files/Google/Chrome/Application/chrome.exe';
const rea = process.env.REA_CLI ??
  resolve(process.env.LOCALAPPDATA ?? '.', 'npm-cache/_npx/da7caeae9db9c589/node_modules/rea-agents/scripts/rea.mjs');
const revision = 'fa649586f35119af9032312ef21e300e41d1c3b6';
const defaultSuites = [
  'readable-streams/async-iterator.any.js',
  'readable-byte-streams/construct-byob-request.any.js',
  'readable-byte-streams/read-min.any.js',
  'readable-byte-streams/respond-after-enqueue.any.js',
  'writable-streams/general.any.js',
  'writable-streams/write.any.js',
  'writable-streams/aborting.any.js',
  'piping/then-interception.any.js',
  'piping/general-addition.any.js',
  'piping/multiple-propagation.any.js',
  'transform-streams/general.any.js',
];
const suites = filter.length ? filter.filter(name => name !== 'fixture') : defaultSuites;
const checkFixture = filter.length === 0 || filter.includes('fixture');
for (const file of suites) assert.match(file, /^[a-z-]+\/[a-z-]+\.any\.js$/);
await mkdir(output, { recursive: true });
const save = (path, value) => writeFile(resolve(output, path), JSON.stringify(value, null, 2) + '\n');
const sha256 = bytes => createHash('sha256').update(bytes).digest('hex');
const fixture = await readFile(new URL('./fixtures/web-streams/index.html', import.meta.url));
const harness = await readFile(new URL('./fixtures/web-streams/testharness.js', import.meta.url));
const expected = {
  fetch: [true, false, 2, '90,91,11,12,13,95', 204, false, '14,15,16', true, 0, false],
  pipe: ['8,9,10,99,closed', false, false, false, false],
  blob: ['21,22,23', false, false, '21,22,23', true, 0, '21,22,23'],
};
const report = {
  binary: mode === 'chrome' ? null : binary,
  fixtureSha256: sha256(fixture), harnessSha256: sha256(harness), revision,
  runnerSha256: sha256(await readFile(fileURLToPath(import.meta.url))),
  upstream: {}, runs: {}, errors: [],
};

const assets = new Map();
async function loadPinned(file) {
  if (assets.has(file)) return assets.get(file);
  assert.match(file, /^[a-z-]+\/[\w.-]+\.js$/);
  const url = 'https://raw.githubusercontent.com/web-platform-tests/wpt/' + revision + '/streams/' + file;
  const response = await fetch(url, { signal: AbortSignal.timeout(20000) });
  assert.ok(response.ok, 'WPT ' + file + ' returned ' + response.status);
  const bytes = Buffer.from(await response.arrayBuffer());
  assets.set(file, bytes);
  report.upstream[file] = sha256(bytes);
  return bytes;
}

const pages = new Map();
for (const suite of suites) {
  const source = (await loadPinned(suite)).toString('utf8');
  const scripts = [...source.matchAll(/^\/\/ META: script=(.+)$/gm)].map(match =>
    posix.normalize(posix.join(posix.dirname(suite), match[1].trim())));
  await Promise.all(scripts.map(loadPinned));
  const html = '<!doctype html><meta charset=utf-8><title>Streams WPT</title>' +
    '<pre id="wpt-receipt"></pre><script src="/harness.js"></script>' +
    [...scripts, suite].map(script => '<script src="/upstream/' + script + '"></script>').join('') +
    '<script>runStreamsTests();</script>';
  pages.set('/wpt/' + suite, html);
}

const tests = new Map(['rust-headless', 'rust-native', 'chrome'].map(name => [name, {
  order: [], peer: null, receipt: null,
}]));
const htmlType = 'text/html; charset=utf-8';
const jsType = 'text/javascript; charset=utf-8';
function reply(response, code, type, body) {
  response.writeHead(code, { 'Content-Type': type, 'Cache-Control': 'no-store' });
  response.end(body);
}

async function serve(request, response) {
  const path = new URL(request.url, 'http://127.0.0.1').pathname;
  if (pages.has(path)) return reply(response, 200, htmlType, pages.get(path));
  if (path === '/harness.js') return reply(response, 200, jsType, harness);
  const asset = assets.get(path.slice('/upstream/'.length));
  if (path.startsWith('/upstream/') && asset) return reply(response, 200, jsType, asset);
  const [_, name, route] = /^\/fixture\/([^/]+)\/([^/]+)$/.exec(path) ?? [];
  const test = tests.get(name);
  if (!test) return reply(response, 404, 'text/plain', 'Unknown fixture path');
  switch (route) {
    case 'index.html':
      return reply(response, 200, htmlType, fixture);
    case 'pieces':
      assert.equal(test.peer, null);
      test.peer = response;
      test.order.push('first bytes');
      response.writeHead(200, { 'Content-Type': 'application/octet-stream', 'Content-Length': '6' });
      response.write(Buffer.from([11, 12, 13]));
      return;
    case 'resume':
      assert.equal(request.method, 'POST');
      assert.ok(test.peer, 'The first BYOB chunk must arrive while the response is open');
      test.peer.end(Buffer.from([14, 15, 16]));
      test.peer = null;
      test.order.push('remaining bytes');
      return reply(response, 204, 'text/plain');
    case 'relay':
      test.order.push('pipe input');
      return reply(response, 200, 'application/octet-stream', Buffer.from([7, 8, 9]));
    case 'receipt': {
      assert.equal(request.method, 'POST');
      assert.equal(test.receipt, null);
      const chunks = [];
      let size = 0;
      for await (const chunk of request) {
        size += chunk.length;
        assert.ok(size < 8192);
        chunks.push(chunk);
      }
      test.receipt = JSON.parse(Buffer.concat(chunks).toString('utf8'));
      test.order.push('receipt');
      return reply(response, 204, 'text/plain');
    }
    default:
      return reply(response, 404, 'text/plain', 'Unknown fixture route');
  }
}

const server = createServer((request, response) => {
  serve(request, response).catch(error => {
    report.errors.push({ name: 'Fixture server', error: String(error) });
    if (response.headersSent) response.destroy();
    else reply(response, 500, 'text/plain', 'Fixture server error');
  });
});
server.listen(0, '127.0.0.1');
await once(server, 'listening');
const origin = 'http://127.0.0.1:' + server.address().port;
const fixtureUrl = name => origin + '/fixture/' + name + '/index.html';

async function command(exe, args, stem, json = true) {
  const child = spawn(exe, args, { windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
  let stdout = '', stderr = '', expired = false;
  child.stdout.setEncoding('utf8').on('data', value => { stdout += value; });
  child.stderr.setEncoding('utf8').on('data', value => { stderr += value; });
  const deadline = setTimeout(() => { expired = true; child.kill(); }, 50000);
  try {
    const [exitCode] = await once(child, 'close');
    assert.ok(!expired, stem + ' timed out');
    assert.equal(exitCode, 0, stem + ' failed. ' + stderr.slice(0, 800));
    return json ? JSON.parse(stdout) : stdout.trim();
  } finally {
    clearTimeout(deadline);
    await Promise.all([
      writeFile(resolve(output, stem + '.stdout.txt'), stdout),
      writeFile(resolve(output, stem + '.stderr.txt'), stderr),
    ]);
  }
}

async function attempt(name, action) {
  try { return await action(); } catch (error) {
    report.errors.push({ name, error: String(error) });
    console.error(name + ' ' + String(error).slice(0, 1200));
  }
}

const entities = { amp: '&', lt: '<', gt: '>', quot: '"', '#39': "'" };
const chromeDifferences = [
  ['readable-streams-async-iterator.any.js', 'Async iterator instances should have the correct list of properties',
    'return should have 1 parameter (expected 1, got 0)'],
  ['transform-streams-general.any.js', 'terminate() should abort writable immediately after readable.cancel()',
    'closed should reject with TypeError'],
];
const rustDifferences = [
  ['transform-streams-general.any.js', 'terminate() should abort writable immediately after readable.cancel()',
    'TypeError: TransformStream was terminated'],
];

function verifyWpt(browser, suite, dom, entry) {
  const stem = browser + '-' + suite;
  const match = /<pre\b([^>]*\bid="wpt-receipt"[^>]*)>([^<]*)<\/pre>/.exec(dom);
  if (match?.[2]) entry.subtests = JSON.parse(match[2].replace(/&(amp|lt|gt|quot|#39);/g, (_, key) => entities[key]));
  assert.ok(match?.[1].includes('data-complete="yes"'), stem + ' did not finish its suite');
  assert.ok(entry.subtests?.length, stem + ' reported no tests');
  const failures = entry.subtests.filter(test => test.status !== 'pass');
  const expectedDifferences = browser === 'chrome' ? chromeDifferences : rustDifferences;
  const differences = failures.filter(test => expectedDifferences.some(([file, name, detail]) =>
    suite === file && test.name === name && test.detail?.includes(detail)));
  if (differences.length) entry.knownDifferences = differences;
  assert.equal(failures.length, differences.length, JSON.stringify(failures));
}

function verifyFixture(name, entry) {
  const { order, receipt } = tests.get(name);
  Object.assign(entry, { order, receipt });
  assert.deepEqual(receipt, expected);
  assert.deepEqual(order, ['first bytes', 'remaining bytes', 'pipe input', 'receipt']);
}

async function rustHeadless(url, stem, entry) {
  const domPath = resolve(output, stem + '.dom.html');
  entry.result = await command(binary, [
    '--headless', '--url', url, '--profile', resolve(output, 'profile-' + randomUUID()),
    '--dump-dom', domPath, '--run-for-ms', '11500',
  ], stem);
  return readFile(domPath, 'utf8');
}

async function chromeHeadless(url, selector, stem, entry) {
  const path = stem + '.scenario.json';
  await save(path, {
    browser: { mode: 'launch', executable_path: chrome, headless: true },
    start_url: { url },
    actions: [{
      step_id: 'receipt', action: 'wait_for', locator: { kind: 'css', selector },
      state: 'attached', timeout_ms: 18000,
    }],
    capture: { at_end: ['url', 'dom'], events: ['page-errors'] },
    command_timeout_ms: 30000,
  });
  const result = await command(process.execPath, [
    rea, 'capture-browser-scenario', resolve(output, path), '--format', 'json', '--full-output',
  ], stem);
  assert.ok(result.ok, JSON.stringify(result.errors ?? []));
  const capture = result.data.normalized_result;
  entry.evidence = { id: result.data.evidence_id, browser: capture.browser };
  assert.equal(capture.browser.process_ownership, 'provider-owned');
  assert.equal(capture.browser.cleanup, 'terminated-owned-process');
  const last = capture.steps.at(-1);
  assert.equal(last.status, 'completed', JSON.stringify(last.error));
  assert.equal(last.artifacts.dom.state, 'captured');
  const dom = last.artifacts.dom.value.text;
  await writeFile(resolve(output, stem + '.dom.html'), dom);
  return dom;
}

async function runRust() {
  report.binarySha256 = sha256(await readFile(binary));
  report.version = await command(binary, ['--version'], 'rust-version', false);
  const run = report.runs.rust = { wpt: {} };
  if (checkFixture) {
    await attempt('Rust headless fixture', async () => {
      const entry = run.headless = {};
      const dom = await rustHeadless(fixtureUrl('rust-headless'), 'rust-headless', entry);
      verifyFixture('rust-headless', entry);
      assert.match(dom, /<pre\b(?=[^>]*\bid="receipt")(?=[^>]*\bdata-complete="yes")/);
      assert.deepEqual(entry.result.javascript.diagnostics, []);
      assert.equal(entry.result.layout_limited, false);
      assert.equal(entry.result.paint_limited, false);
    });
    await attempt('Rust native fixture', async () => {
      const entry = run.native = {};
      await save('native-actions.json', [{ action: 'title', value: 'Web streams verified' }]);
      entry.result = await command(binary, [
        '--url', fixtureUrl('rust-native'), '--profile', resolve(output, 'profile-' + randomUUID()),
        '--smoke-window', resolve(output, 'rust-native.png'),
        '--smoke-actions', resolve(output, 'native-actions.json'),
      ], 'rust-native');
      verifyFixture('rust-native', entry);
      assert.equal(entry.result.window_smoke, true);
      assert.ok(entry.result.frames_presented > 0);
      assert.deepEqual(entry.result.javascript.diagnostics, []);
      assert.equal(entry.result.layout_limited, false);
      assert.equal(entry.result.paint_limited, false);
    });
  }
  for (const suite of suites) await attempt('Rust ' + suite, async () => {
    const stem = 'rust-' + suite.replace('/', '-');
    const entry = run.wpt[suite] = {};
    verifyWpt('rust', suite.replace('/', '-'), await rustHeadless(origin + '/wpt/' + suite, stem, entry), entry);
    assert.deepEqual(entry.result.javascript.diagnostics, []);
  });
  assert.equal(sha256(await readFile(binary)), report.binarySha256, 'Binary changed during verification');
}

async function runChrome() {
  const run = report.runs.chrome = { wpt: {} };
  if (checkFixture) await attempt('Chrome fixture', async () => {
    const entry = run.headless = {};
    await chromeHeadless(fixtureUrl('chrome'), '#receipt[data-complete]', 'chrome-fixture', entry);
    verifyFixture('chrome', entry);
  });
  for (const suite of suites) await attempt('Chrome ' + suite, async () => {
    const stem = 'chrome-' + suite.replace('/', '-');
    const entry = run.wpt[suite] = {};
    const dom = await chromeHeadless(origin + '/wpt/' + suite, '#wpt-receipt[data-complete]', stem, entry);
    verifyWpt('chrome', suite.replace('/', '-'), dom, entry);
  });
}

try {
  if (mode !== 'chrome') await attempt('Rust runtime', runRust);
  if (mode !== 'rust') await attempt('Chrome runtime', runChrome);
  if (mode === 'both') await attempt('WPT parity', () => {
    const names = entry => entry?.subtests?.map(test => test.name);
    for (const suite of suites) assert.deepEqual(names(report.runs.rust?.wpt[suite]),
      names(report.runs.chrome?.wpt[suite]), suite);
  });
} finally {
  const closed = once(server, 'close');
  server.close();
  server.closeAllConnections();
  await closed;
  report.ok = report.errors.length === 0;
  await save('report.json', report);
}
const counts = Object.fromEntries(Object.entries(report.runs).map(([name, run]) => [
  name, Object.fromEntries(Object.entries(run.wpt).map(([suite, entry]) => [
    suite, [entry.subtests?.filter(test => test.status === 'pass').length ?? 0, entry.subtests?.length ?? 0],
  ])),
]));
console.log(JSON.stringify({ ok: report.ok, binarySha256: report.binarySha256, counts, report: resolve(output, 'report.json') }));
if (!report.ok) process.exitCode = 1;
