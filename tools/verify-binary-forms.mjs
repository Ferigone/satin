import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { createServer } from 'node:http';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const args = process.argv.slice(2);
const flags = new Set(args.filter(arg => arg.startsWith('--')));
assert.ok([...flags].every(flag => ['--chrome-reference', '--chrome-reference-only'].includes(flag)),
  'Usage: node tools/verify-binary-forms.mjs [BROWSER.exe] [OUTPUT] [--chrome-reference | --chrome-reference-only]');
const [exe = 'target/release/rust-browser.exe', directory = 'target/verify-binary-forms'] =
  args.filter(arg => !arg.startsWith('--'));
const chromeOnly = flags.has('--chrome-reference-only');
const withChrome = chromeOnly || flags.has('--chrome-reference');
const binary = resolve(exe);
const output = resolve(directory);
const toolDirectory = dirname(fileURLToPath(import.meta.url));
const fixture = await readFile(resolve(toolDirectory, 'fixtures/binary-forms.html'));
const chrome = process.env.CHROME_EXE ??
  'C:/Program Files/Google/Chrome/Application/chrome.exe';
const rea = process.env.REA_CLI ??
  'C:/Users/admin/AppData/Local/npm-cache/_npx/da7caeae9db9c589/node_modules/rea-agents/scripts/rea.mjs';
const sha = bytes => createHash('sha256').update(bytes).digest('hex');
const modes = new Set(['headless', 'native', 'chrome']);
const binaryPayload = Buffer.from([0, 255, 13, 10, 128]);
const expected = {
  phase: 'bindings', blob: true, formData: true, formEvents: 1, bytes: '0,255,13,10,128',
};
const report = { ok: false, fixtureSha256: sha(fixture), expected, modes: {}, requests: [], failures: [] };
const received = new Map();
const title = 'Binary forms verified';
const html = 'text/html; charset=utf-8';
const text = 'text/plain; charset=utf-8';
await mkdir(output, { recursive: true });

function json(filename, value) {
  return writeFile(resolve(output, filename), JSON.stringify(value, null, 2) + '\n');
}

function multipart(boundary) {
  const marker = '--' + boundary;
  const field = (name, value) =>
    marker + '\r\nContent-Disposition: form-data; name="' + name + '"\r\n\r\n' + value + '\r\n';
  const file = (name, filename, mime) =>
    marker + '\r\nContent-Disposition: form-data; name="' + name + '"; filename="' +
      filename + '"\r\nContent-Type: ' + mime + '\r\n\r\n';
  return Buffer.concat([
    Buffer.from(
      field('outside', 'outer') +
      field('token', 'a 🦀') +
      field('memo', 'mañana\r\nline') +
      file('empty', '', 'application/octet-stream') + '\r\n' +
      field('commit', 'send') +
      field('dupe', 'first') +
      field('dupe', 'second') +
      field('quoted%22%0D%0Afield', 'one\r\ntwo\r\nthree') +
      file('upload', 'café.bin', 'application/x-binary')),
    binaryPayload,
    Buffer.from('\r\n' + marker + '--\r\n'),
  ]);
}

async function readBody(request) {
  const chunks = [];
  let bytes = 0;
  for await (const chunk of request) {
    bytes += chunk.length;
    assert.ok(bytes <= 1024 * 1024, 'fixture body limit');
    chunks.push(chunk);
  }
  return Buffer.concat(chunks);
}

function reply(response, code, contentType, body) {
  response.writeHead(code, { 'Content-Type': contentType, 'Cache-Control': 'no-store' });
  response.end(body);
}

function releaseNativeReady(state) {
  if (state.nativeReady) {
    reply(state.nativeReady, 200, text, 'Ready');
    state.nativeReady = null;
    state.nativeReleased = true;
  }
}

async function handle(request, response) {
  const { pathname, searchParams } = new URL(request.url, 'http://127.0.0.1');
  if (pathname === '/favicon.ico') return reply(response, 204, text, '');
  const mode = searchParams.get('mode');
  assert.ok(modes.has(mode), 'unknown fixture mode');
  const state = received.get(mode) ?? {};
  received.set(mode, state);
  const input = {
    mode, path: pathname, method: request.method, contentType: request.headers['content-type'] ?? null,
  };
  report.requests.push(input);
  if (pathname === '/fixture') return reply(response, 200, html, fixture);
  if (pathname === '/native-ready') {
    assert.equal(mode, 'native');
    assert.equal(request.method, 'GET');
    assert.equal(state.nativeReady, undefined, 'duplicate native readiness request');
    // The native smoke runner advances to its next title check once its
    // network queue is idle. Hold headers until the page reports readiness.
    state.nativeReady = response;
    if (state.receipt || state.error) releaseNativeReady(state);
    return;
  }
  if (pathname === '/verified') {
    assert.equal(request.method, 'GET', '303 follow-up method');
    assert.ok(state.submit, '303 follow-up arrived before multipart submission');
    state.landing = true;
    return reply(response, 200, html,
      '<!doctype html><meta charset=utf-8><title>' + title + '</title>' +
      '<main id="verified"><h1>Multipart upload verified</h1>' +
      '<p>The server received the payload before issuing a 303 redirect.</p>' +
      '<dl><dt>Filename</dt><dd>café.bin</dd><dt>Media type</dt><dd>application/x-binary</dd>' +
      '<dt>File bytes</dt><dd>0, 255, 13, 10, 128</dd>' +
      '<dt>Repeated fields</dt><dd>first, second</dd><dt>Landing request</dt><dd>GET</dd></dl></main>');
  }
  assert.equal(request.method, 'POST', pathname + ' method');
  const body = await readBody(request);
  input.length = body.length;
  input.sha256 = sha(body);
  if (pathname === '/binary') {
    assert.equal(input.contentType, 'application/x-binary', 'Blob MIME');
    assert.deepEqual(body, binaryPayload, 'Blob wire bytes');
    state.binary = input;
    return reply(response, 200, input.contentType, body);
  }
  if (pathname === '/receipt') {
    state.receipt = JSON.parse(body.toString('utf8'));
    assert.deepEqual(state.receipt, expected);
    reply(response, 200, text, 'OK');
    releaseNativeReady(state);
    return;
  }
  if (pathname === '/failure') {
    state.error = body.toString('utf8');
    releaseNativeReady(state);
    throw new Error(mode + ' script: ' + state.error);
  }
  assert.ok(['/multipart','/submit'].includes(pathname), 'unknown fixture path');
  const boundary = /^multipart\/form-data; boundary=([A-Za-z0-9_-]{27,70})$/.exec(input.contentType)?.[1];
  assert.ok(boundary, 'multipart boundary in Content-Type');
  const bytes = multipart(boundary);
  await writeFile(resolve(output, mode + '-' + pathname.slice(1) + '.bin'), body);
  assert.deepEqual(body, bytes, pathname + ' ordered multipart wire bytes');
  if (pathname === '/multipart') {
    state.multipart = input;
    return reply(response, 200, input.contentType, body);
  }
  assert.deepEqual(state.receipt, expected, 'form submitted after Fetch checks');
  assert.ok(state.multipart && state.binary);
  assert.notEqual(input.contentType, state.multipart.contentType, 'new boundary for each body');
  state.submit = input;
  response.writeHead(303, { Location: '/verified?mode=' + mode, 'Cache-Control': 'no-store' });
  response.end();
}

const server = createServer((request, response) => {
  handle(request, response).catch(error => {
    report.failures.push(error.stack);
    if (!response.headersSent) reply(response, 500, text, error.message);
    else response.destroy(error);
  });
});
await new Promise((done, fail) => {
  server.once('error', fail);
  server.listen(0, '127.0.0.1', done);
});
const origin = 'http://127.0.0.1:' + server.address().port;
const page = mode => origin + '/fixture?mode=' + mode;
const profile = mode => resolve(output, mode + '-profile');

async function command(executable, args, name, parse = true) {
  const child = spawn(executable, args, { windowsHide: true, stdio: ['ignore','pipe','pipe'] });
  let stdout = '', stderr = '';
  child.stdout.setEncoding('utf8').on('data', chunk => { stdout += chunk; });
  child.stderr.setEncoding('utf8').on('data', chunk => { stderr += chunk; });
  const timeout = setTimeout(() => child.kill(), 45000);
  let code;
  try {
    code = await new Promise((done, fail) => {
      child.once('error', fail);
      child.once('close', done);
    });
  } finally {
    clearTimeout(timeout);
    await writeFile(resolve(output, name + '.stdout.txt'), stdout);
    await writeFile(resolve(output, name + '.stderr.txt'), stderr);
  }
  assert.equal(code, 0, name + ' exited ' + code + '\n' + stderr.trim() + '\n' + stdout.slice(-2000));
  return parse ? JSON.parse(stdout) : stdout.trim();
}

function success(mode) {
  const record = received.get(mode) ?? {};
  assert.deepEqual(record.receipt, expected, mode + ' receipt');
  assert.ok(record.binary && record.multipart && record.submit && record.landing, mode + ' requests');
  assert.equal(record.error, undefined, mode + ' script failure');
  if (mode === 'native') assert.equal(record.nativeReleased, true);
  report.modes[mode].receipt = record.receipt;
  report.modes[mode].passed = true;
}

async function attempt(mode, callback) {
  try {
    await callback();
  } catch (error) {
    report.failures.push(mode + ': ' + error.stack);
  }
}

async function rust() {
  report.binarySha256 = sha(await readFile(binary));
  report.version = await command(binary, ['--version'], 'version', false);
  await attempt('headless', async () => {
    const result = await command(binary, [
      '--headless', '--url', page('headless'), '--profile', profile('headless'),
      '--click-id', 'run', '--run-for-ms', '12000', '--dump-dom', resolve(output, 'rust-headless.html'),
    ], 'headless');
    report.modes.headless = { result };
    assert.equal(result.title, title, JSON.stringify(result.javascript));
    assert.deepEqual(result.javascript.diagnostics, []);
    success('headless');
  });
  await attempt('native', async () => {
    const actions = [
      { action:'title', value:'Binary forms ready' },
      { action:'click', id:'run' },
      { action:'title', value:'Binary forms can submit' },
      { action:'click', id:'send' },
      { action:'title', value:title },
    ];
    await json('native-actions.json', actions);
    const image = resolve(output, 'rust-native.png');
    const result = await command(binary, [
      '--url', page('native'), '--profile', profile('native'),
      '--smoke-window', image, '--smoke-actions', resolve(output, 'native-actions.json'),
    ], 'native');
    assert.equal(result.title, title, JSON.stringify(result.javascript));
    assert.equal(result.window_smoke, true);
    assert.ok(result.frames_presented > 0);
    assert.deepEqual(result.javascript.diagnostics, []);
    const png = await readFile(image);
    assert.equal(png.subarray(0, 8).toString('hex'), '89504e470d0a1a0a');
    report.modes.native = { result, imageSha256: sha(png) };
    success('native');
  });
  report.binaryFinalSha256 = sha(await readFile(binary));
  assert.equal(report.binaryFinalSha256, report.binarySha256, 'executable changed during verification');
}

const wait = (step_id, selector) => ({
  step_id, action:'wait_for', state:'attached', timeout_ms:12000,
  locator:{kind:'css',selector},
});
const click = (step_id, selector) => ({
  step_id, action:'click', timeout_ms:8000, locator:{kind:'css',selector},
});

async function reference() {
  await json('chrome-scenario.json', {
    browser: { mode:'launch', executable_path:chrome, headless:true },
    start_url: { url:page('chrome') },
    actions: [
      wait('ready', '#run[data-ready]'),
      click('run', '#run'),
      wait('checks-complete', '#send[data-verified]'),
      click('submit', '#send'),
      wait('redirect-complete', '#verified'),
    ],
    capture: { at_end:['url','dom'], events:['page-errors','network'] },
    command_timeout_ms: 34000,
  });
  const run = await command(process.execPath, [
    rea, 'capture-browser-scenario', resolve(output, 'chrome-scenario.json'),
    '--format', 'json', '--full-output',
  ], 'chrome');
  const result = run.data.normalized_result;
  const cleanup = result.browser.cleanup;
  report.modes.chrome = { evidence: run.data.evidence_id, browser: result.browser };
  assert.equal(result.browser.process_ownership, 'provider-owned');
  assert.equal(cleanup, 'terminated-owned-process');
  assert.ok(result.steps.every(step => step.status === 'completed'),
    JSON.stringify(result.steps.map(step => [step.step_id, step.status, step.error])));
  const dom = result.steps.at(-1).artifacts.dom;
  assert.equal(dom.state, 'captured');
  assert.match(dom.value.text, /<title>Binary forms verified<\/title>/);
  await writeFile(resolve(output, 'chrome.dom.html'), dom.value.text);
  success('chrome');
}

try {
  if (!chromeOnly) await attempt('Rust runtime', rust);
  if (withChrome) await attempt('Chrome reference', reference);
} finally {
  server.closeAllConnections();
  await new Promise(done => server.close(done));
  report.ok = report.failures.length === 0;
  await json('report.json', report);
}

console.log(JSON.stringify({
  ok: report.ok, report: resolve(output, 'report.json'),
  modes: Object.fromEntries(Object.entries(report.modes).map(([mode, run]) => [mode, run.passed ?? false])),
  failures: report.failures,
}));
if (!report.ok) process.exitCode = 1;
