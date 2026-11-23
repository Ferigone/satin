// Exercise the navigator, async JavaScript, Fetch, and socket cancellation.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { once } from 'node:events';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const repo = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const usage = 'Usage: node tools/verify-async-network.mjs [BROWSER.exe] [OUTPUT] [--chrome-reference | --chrome-reference-only] [--rea PATH] [--chrome PATH]';
const args = process.argv.slice(2);
const positional = [];
const options = new Map();
for (let index = 0; index < args.length; index++) {
  const arg = args[index];
  if (arg === '--chrome-reference' || arg === '--chrome-reference-only') {
    options.set(arg, true);
  } else if (arg === '--rea' || arg === '--chrome') {
    assert.ok(args[index + 1] && !args[index + 1].startsWith('--'), usage);
    options.set(arg, args[++index]);
  } else {
    assert.ok(!arg.startsWith('--'), usage);
    positional.push(arg);
  }
}
assert.ok(positional.length <= 2, usage);
const chromeOnly = options.has('--chrome-reference-only');
const withChrome = chromeOnly || options.has('--chrome-reference');
const binary = resolve(positional[0] ?? resolve(repo, 'target/debug/rust-browser.exe'));
const output = resolve(positional[1] ?? resolve(repo, 'target/async-network-verification'));
const rea = resolve(options.get('--rea') ?? process.env.REA_CLI ?? resolve(repo, 'node_modules/rea-agents/scripts/rea.mjs'));
const chrome = resolve(options.get('--chrome') ?? resolve(process.env.ProgramFiles ?? 'C:/Program Files', 'Google/Chrome/Application/chrome.exe'));
await mkdir(output, { recursive: true });

const fixture = await readFile(new URL('./fixtures/async-network.html', import.meta.url));
const sha256 = bytes => createHash('sha256').update(bytes).digest('hex');
const writeJson = (name, value) => writeFile(resolve(output, name), JSON.stringify(value, null, 2) + '\n');
const expected = {
  search: '?q=a+b%2B%C3%A9%26%3D&q=two&mode=%7E*',
  body: 'search=a+b&search=%C5%81%C3%B3d%C5%BA+%F0%9F%A6%80&literal=%2B%26%3D%25%7E*',
  contentType: 'application/x-www-form-urlencoded;charset=utf-8',
  closeDeadlineMs: 2000,
};
const parts = ['headers', 'body', 'reader'];
const partial = 'partial body delivered';
const download = Buffer.alloc(5 * 1024 * 1024 + 2, 0x9b);
download[0] = 72;
download[download.length - 1] = 33;
const report = { ok: false, binary: chromeOnly ? null : binary, fixtureSha256: sha256(fixture), expected,
  runs: {}, processes: {}, requests: [], trace: [], failures: [] };
if (!chromeOnly) report.binarySha256 = sha256(await readFile(binary));
const startedAt = performance.now();
const elapsed = () => performance.now() - startedAt;
const note = (mode, event, details = {}) => {
  const atMs = elapsed();
  report.trace.push({ mode, event, atMs, ...details });
  return atMs;
};
function latch() {
  let resolve;
  const promise = new Promise(ready => { resolve = ready; });
  return { promise, resolve };
}
const runs = new Map(['headless', 'native', 'chrome'].map(mode => [mode, {
  receipt: null,
  submit: null,
  stalls: new Map(parts.map(part => [part, { request: null, started: latch(), closed: latch(),
    gates: new Set(), startedAt: null, releaseAt: null, closedAt: null, sentBytes: 0 }])),
}]));
const socketIds = new WeakMap();
let nextSocketId = 0;
let origin;

async function within(promise, milliseconds, message) {
  let timer;
  try {
    return await Promise.race([
      promise,
      new Promise((_, reject) => { timer = setTimeout(() => reject(new Error(message)), milliseconds); }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

function reply(response, row, status, type, body = '') {
  if (response.destroyed) return;
  row.status = status;
  response.writeHead(status, { 'Content-Type': type, 'Cache-Control': 'no-store' });
  response.end(body);
}

async function requestBody(request) {
  const chunks = [];
  let length = 0;
  for await (const chunk of request) {
    length += chunk.length;
    assert.ok(length <= 8192, 'Fixture request body exceeds 8 KiB');
    chunks.push(chunk);
  }
  return Buffer.concat(chunks).toString('utf8');
}

function stall(mode, part, current, request, response, row) {
  assert.equal(request.method, 'GET');
  assert.equal(current.request, null, 'Duplicate stall for ' + part);
  current.request = row;
  request.socket.once('close', hadError => {
    current.closedAt = note(mode, 'stall-closed', { part, hadError, socket: row.socket,
      responseEnded: response.writableEnded });
    current.closed.resolve();
  });
  if (part === 'headers') {
    assert.equal(response.headersSent, false);
    current.startedAt = note(mode, 'stall-awaiting-headers', { part, socket: row.socket });
    current.started.resolve();
    return;
  }
  row.status = 200;
  response.writeHead(200, { 'Content-Type': 'text/plain', 'Cache-Control': 'no-store', 'Content-Length': '4096' });
  response.flushHeaders();
  response.write(partial, error => {
    if (error) {
      note(mode, 'partial-write-failed', { part, error: String(error) });
      return;
    }
    current.sentBytes = Buffer.byteLength(partial);
    current.startedAt = note(mode, 'stall-partial-body', { part, bytes: current.sentBytes, socket: row.socket });
    current.started.resolve();
  });
}

async function gate(mode, part, phase, current, request, response, row) {
  assert.equal(request.method, 'GET');
  assert.ok(!current.gates.has(phase), 'Duplicate ' + phase + ' gate for ' + part);
  current.gates.add(phase);
  if (phase === 'started') {
    await within(current.started.promise, 3000, part + ' stall never began');
    assert.equal(current.closedAt, null, part + ' socket closed before abort could run');
    current.releaseAt = note(mode, 'release-abort', { part, socket: current.request.socket });
  } else {
    assert.ok(current.releaseAt !== null, part + ' closed gate arrived without abort release');
    const remaining = Math.ceil(expected.closeDeadlineMs - (elapsed() - current.releaseAt));
    await within(current.closed.promise, Math.max(0, remaining), part + ' socket did not close before deadline');
    assert.ok(current.closedAt > current.releaseAt, part + ' socket closed before abort release');
    assert.ok(current.closedAt - current.releaseAt <= expected.closeDeadlineMs, part + ' socket closed after deadline');
    note(mode, 'close-confirmed', { part, socket: current.request.socket });
  }
  reply(response, row, 200, 'text/plain', phase);
}

async function serve(request, response, row) {
  const uri = new URL(request.url, origin);
  const match = /^\/async-network\/(headless|native|chrome)\/(index\.html|never|stall|gate|download|submit|receipt)$/.exec(uri.pathname);
  if (!match) return reply(response, row, 404, 'text/plain', 'Unknown fixture route');
  const [, mode, route] = match;
  const state = runs.get(mode);
  row.mode = mode;
  row.route = route;
  if (route === 'index.html') {
    assert.equal(request.method, 'GET');
    return reply(response, row, 200, 'text/html; charset=utf-8', fixture);
  }
  if (route === 'never') throw new Error(mode + ' preaborted Fetch reached the server');
  if (route === 'download') {
    assert.equal(request.method, 'GET');
    response.setHeader('Content-Length', download.length);
    return reply(response, row, 200, 'application/octet-stream', download);
  }
  if (route === 'stall' || route === 'gate') {
    const part = uri.searchParams.get('part');
    const current = state.stalls.get(part);
    assert.ok(current, 'Unknown stall part');
    if (route === 'stall') {
      assert.equal(uri.search, '?part=' + part);
      return stall(mode, part, current, request, response, row);
    }
    const phase = uri.searchParams.get('phase');
    assert.ok(phase === 'started' || phase === 'closed', 'Unknown gate phase');
    assert.equal(uri.search, '?part=' + part + '&phase=' + phase);
    return gate(mode, part, phase, current, request, response, row);
  }

  assert.equal(request.method, 'POST');
  row.body = await requestBody(request);
  if (route === 'submit') {
    assert.equal(state.submit, null, 'Duplicate form submission');
    for (const [part, current] of state.stalls) {
      assert.ok(current.closedAt && current.gates.has('closed'), part + ' socket must close before POST');
      assert.ok(current.closedAt < row.atMs, part + ' socket closed after POST arrived');
    }
    assert.equal(uri.search, expected.search);
    assert.equal(row.body, expected.body);
    assert.equal(row.contentType?.toLowerCase(), expected.contentType);
    state.submit = { search: uri.search, body: row.body, contentType: row.contentType };
    note(mode, 'submit-accepted');
    return reply(response, row, 200, 'application/json', '{"accepted":true}');
  }
  assert.equal(state.receipt, null, 'Duplicate final receipt');
  assert.ok(state.submit, 'Receipt arrived before successful form POST');
  state.receipt = JSON.parse(row.body);
  note(mode, 'receipt-accepted');
  return reply(response, row, 204, 'text/plain');
}

const server = createServer((request, response) => {
  const row = {
    method: request.method, url: request.url, socket: socketIds.get(request.socket), atMs: elapsed(),
    contentType: request.headers['content-type'] ?? null, status: null,
  };
  report.requests.push(row);
  serve(request, response, row).catch(error => {
    report.failures.push({ name: row.mode ?? 'server', error: String(error) });
    reply(response, row, 500, 'text/plain', 'Fixture failed');
  });
});
server.on('connection', socket => socketIds.set(socket, ++nextSocketId));
server.listen(0, '127.0.0.1');
await once(server, 'listening');
origin = 'http://127.0.0.1:' + server.address().port;
report.origin = origin;

const url = mode => origin + '/async-network/' + mode + '/index.html';
const freshProfile = mode => resolve(output, 'qa-' + mode + '-' + randomUUID());

async function runProcess(exe, argv, stem, deadline) {
  const process = { executable: exe, args: argv, deadline, exitCode: null, signal: null, timedOut: false };
  report.processes[stem] = process;
  const child = spawn(exe, argv, { windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
  let stdout = '';
  let stderr = '';
  child.stdout.setEncoding('utf8').on('data', chunk => { stdout += chunk; });
  child.stderr.setEncoding('utf8').on('data', chunk => { stderr += chunk; });
  const timer = setTimeout(() => { process.timedOut = true; child.kill(); }, deadline);
  try {
    [process.exitCode, process.signal] = await once(child, 'close');
  } finally {
    clearTimeout(timer);
    await Promise.all([
      writeFile(resolve(output, stem + '-stdout.json'), stdout),
      writeFile(resolve(output, stem + '-stderr.txt'), stderr),
    ]);
  }
  assert.equal(process.timedOut, false, stem + ' exceeded process deadline');
  assert.equal(process.exitCode, 0, stem + ': ' + stderr.slice(0, 2000));
  return JSON.parse(stdout);
}

async function attempt(name, check) {
  try {
    await check();
  } catch (error) {
    report.failures.push({ name, error: String(error.stack ?? error) });
    console.error(name + ': ' + error.message);
  }
}

function verifyDOM(dom) {
  assert.match(dom, /<title>Async network OK<\/title>/);
  const heading = /<h1\b[^>]*\bid="outcome"[^>]*>Async network OK<\/h1>/.exec(dom);
  assert.ok(heading && heading[0].includes('data-complete="yes"'), 'The final page heading is missing');
  const receipt = /<pre\b[^>]*\bid="receipt"[^>]*>([^<]*)<\/pre>/.exec(dom);
  assert.ok(receipt, 'The page receipt is missing');
  const entities = { amp: '&', lt: '<', gt: '>', quot: '"', '#39': "'" };
  return JSON.parse(receipt[1].replace(/&(amp|lt|gt|quot|#39);/g, (_, entity) => entities[entity]));
}

function verifyServer(mode, domReceipt) {
  const state = runs.get(mode);
  const rows = report.requests.filter(row => row.mode === mode);
  assert.equal(rows.filter(row => row.route === 'never').length, 0);
  assert.equal(rows.filter(row => row.route === 'index.html').length, 1);
  assert.equal(rows.filter(row => row.route === 'stall').length, 3);
  assert.equal(rows.filter(row => row.route === 'gate' && row.status === 200).length, 6);
  assert.equal(rows.filter(row => row.route === 'submit' && row.status === 200).length, 1);
  assert.equal(rows.filter(row => row.route === 'receipt' && row.status === 204).length, 1);
  assert.equal(rows.filter(row => row.route === 'download' && row.status === 200).length, 1);
  const receipt = state.receipt;
  const evidence = report.runs[mode];
  evidence.receipt = receipt;
  evidence.submit = state.submit;
  assert.ok(receipt, mode + ' browser did not return a receipt');
  if (domReceipt) assert.deepEqual(domReceipt, receipt);
  assert.equal(receipt.preabortMatched, true);
  assert.equal(receipt.accepted, true);
  assert.equal(receipt.search, expected.search);
  assert.equal(receipt.serialized, expected.body);
  assert.deepEqual(receipt.download, [download.length, download[0], download[download.length - 1]]);
  evidence.stalls = {};
  for (const [part, current] of state.stalls) {
    if (part !== 'reader') {
      assert.equal(receipt[part].name, 'AbortError');
      assert.equal(receipt[part].signalAborted, true);
      assert.equal(receipt[part].sameReason, true);
      assert.equal(receipt[part].responseExposed, part === 'body');
      assert.equal(receipt[part].prefix, part === 'headers' ? null : partial);
    } else {
      assert.deepEqual(receipt.clones, [partial, partial]);
    }
    assert.ok(current.releaseAt >= current.startedAt && current.closedAt > current.releaseAt);
    assert.equal(current.sentBytes, part === 'headers' ? 0 : Buffer.byteLength(partial));
    assert.equal(current.request.status, part === 'headers' ? null : 200);
    const close = report.trace.find(event => event.mode === mode && event.event === 'stall-closed' && event.part === part);
    assert.ok(close && !close.responseEnded, part + ' socket must close with its response incomplete');
    evidence.stalls[part] = { socket: current.request.socket, sentBytes: current.sentBytes,
      closedAfterReleaseMs: current.closedAt - current.releaseAt };
  }
  assert.equal(new Set(parts.map(part => evidence.stalls[part].socket)).size, parts.length);
  evidence.passed = true;
}

function verifyRust(result) {
  assert.equal(result.title, 'Async network OK', JSON.stringify(result));
  assert.equal(result.layout_limited, false);
  assert.equal(result.paint_limited, false);
  assert.deepEqual(result.javascript.diagnostics, []);
}

async function runRust() {
  report.binarySha256 = sha256(await readFile(binary));
  await attempt('Rust headless', async () => {
    const domPath = resolve(output, 'rust-headless.html');
    const image = resolve(output, 'rust-headless.png');
    const result = await runProcess(binary, [
      '--headless', '--url', url('headless'), '--profile', freshProfile('headless'),
      '--run-for-ms', '12000', '--dump-dom', domPath, '--screenshot', image,
    ], 'rust-headless', 24_000);
    report.runs.headless = { result, imageSha256: sha256(await readFile(image)) };
    verifyRust(result);
    verifyServer('headless', verifyDOM(await readFile(domPath, 'utf8')));
  });
  await attempt('Rust hidden native window', async () => {
    const image = resolve(output, 'rust-native.png');
    await writeJson('native-actions.json', [{ action: 'title', value: 'Async network OK' }]);
    const result = await runProcess(binary, [
      '--url', url('native'), '--profile', freshProfile('native'), '--smoke-window', image,
      '--smoke-actions', resolve(output, 'native-actions.json'),
    ], 'rust-native', 24_000);
    const pixels = await readFile(image);
    report.runs.native = { result, imageSha256: sha256(pixels) };
    verifyRust(result);
    assert.equal(result.window_smoke, true);
    assert.ok(result.frames_presented > 0);
    assert.equal(pixels.subarray(0, 8).toString('hex'), '89504e470d0a1a0a');
    verifyServer('native');
  });
}

async function runChrome() {
  const scenario = {
    browser: { mode: 'launch', executable_path: chrome, headless: true },
    start_url: { url: url('chrome') },
    actions: [{
      step_id: 'async-network-receipt', action: 'wait_for', timeout_ms: 15000,
      locator: { kind: 'css', selector: '#outcome[data-complete]' }, state: 'attached',
    }],
    capture: { at_end: ['url', 'dom'], events: ['page-errors', 'network'] },
    command_timeout_ms: 30000,
  };
  await writeJson('chrome-scenario.json', scenario);
  const result = await runProcess(process.execPath, [
    rea, 'capture-browser-scenario', resolve(output, 'chrome-scenario.json'), '--format', 'json', '--full-output',
  ], 'chrome-rea', 45_000);
  assert.equal(result.ok, true);
  const capture = result.data.normalized_result;
  assert.equal(capture.browser.process_ownership, 'provider-owned');
  assert.equal(capture.browser.cleanup, 'terminated-owned-process');
  report.runs.chrome = { evidenceId: result.data.evidence_id, browser: capture.browser, limitations: result.data.limitations };
  assert.ok(capture.steps.every(step => step.status === 'completed'));
  const dom = capture.steps.at(-1).artifacts.dom;
  assert.equal(dom.state, 'captured');
  await writeFile(resolve(output, 'chrome-final.html'), dom.value.text);
  verifyServer('chrome', verifyDOM(dom.value.text));
}

try {
  if (!chromeOnly) await attempt('Rust binary', runRust);
  if (withChrome) await attempt('Owned Chrome via REA', runChrome);
} finally {
  note('server', 'cleanup');
  report.ok = report.failures.length === 0;
  const closed = once(server, 'close');
  server.close();
  server.closeAllConnections();
  await closed;
  await Promise.all([writeJson('report.json', report), writeFile(resolve(output, 'fixture.html'), fixture)]);
}
if (!report.ok) process.exitCode = 1;
console.log(JSON.stringify({ ok: report.ok, runs: Object.fromEntries(Object.entries(report.runs)
  .map(([mode, run]) => [mode, { passed: run.passed ?? false, stalls: run.stalls, receipt: run.receipt, evidenceId: run.evidenceId }])),
  report: resolve(output, 'report.json') }));
