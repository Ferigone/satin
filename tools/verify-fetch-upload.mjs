import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash, randomUUID, X509Certificate } from 'node:crypto';
import { once } from 'node:events';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { createSecureServer } from 'node:http2';
import { basename, dirname, posix, resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { fileURLToPath } from 'node:url';

const repo = fileURLToPath(new URL('../', import.meta.url));
const [binaryArg, outputArg, mode = 'both', testArg] = process.argv.slice(2);
assert.ok(['rust', 'chrome', 'both'].includes(mode),
  'Usage: node tools/verify-fetch-upload.mjs [BROWSER.exe] [OUTPUT] [both|rust|chrome] [ENGINE_TEST.exe]');
const binary = resolve(binaryArg ?? resolve(repo, 'target/release/rust-browser.exe'));
const engineTest = testArg && resolve(testArg);
const output = resolve(outputArg ?? resolve(repo, 'target/fetch-upload-verification'));
const chrome = process.env.CHROME_EXE ?? 'C:/Program Files/Google/Chrome/Application/chrome.exe';
const rea = process.env.REA_CLI ?? resolve(process.env.LOCALAPPDATA ?? '.',
  'npm-cache/_npx/da7caeae9db9c589/node_modules/rea-agents/scripts/rea.mjs');
const revision = 'fa649586f35119af9032312ef21e300e41d1c3b6';
const wpt = [
  { file: 'fetch/api/request/request-init-stream.any.js', cases: 23 },
  { file: 'fetch/api/request/request-clone-readable-stream-body.any.js', cases: 1 },
  { file: 'fetch/api/basic/request-upload.any.js', cases: 18 },
  { file: 'fetch/api/basic/request-upload.h2.any.js', cases: 12, h2: true },
];
const fixtureFile = resolve(repo, 'tools/fixtures/fetch-upload.html');
const certificates = resolve(repo, 'crates/engine/tests/fixtures/fetch-upload');
const [fixture, harness, cert, key] = await Promise.all([
  readFile(fixtureFile),
  readFile(resolve(repo, 'tools/fixtures/web-streams/testharness.js')),
  readFile(resolve(certificates, 'localhost.pem')),
  readFile(resolve(certificates, 'localhost-key.pem')),
]);
await mkdir(output, { recursive: true });
const save = (name, value) => writeFile(resolve(output, name), JSON.stringify(value, null, 2) + '\n');
const sha256 = (bytes, encoding = 'hex') => createHash('sha256').update(bytes).digest(encoding);
const spki = sha256(new X509Certificate(cert).publicKey.export({ type: 'spki', format: 'der' }), 'base64');
const report = {
  revision, fixtureSha256: sha256(fixture), harnessSha256: sha256(harness), tlsLeafSha256: sha256(cert),
  runnerSha256: sha256(await readFile(fileURLToPath(import.meta.url))),
  pinned: {}, rust: {}, chrome: {}, errors: [],
  h2RustEvidence: 'network::upload::tests exercises the application transport with the local test CA',
};
const pinned = new Map();
const pages = new Map();
const typeHtml = 'text/html; charset=utf-8';
const typeText = 'text/plain; charset=utf-8';
const typeScript = 'text/javascript; charset=utf-8';

function load(file) {
  if (pinned.has(file)) return pinned.get(file);
  const loaded = (async () => {
    const url = 'https://raw.githubusercontent.com/web-platform-tests/wpt/' + revision + '/' + file;
    const response = await fetch(url, { signal: AbortSignal.timeout(20000) });
    assert.ok(response.ok, 'WPT ' + file + ' returned ' + response.status);
    const bytes = Buffer.from(await response.arrayBuffer());
    const path = resolve(output, 'pinned', file);
    await mkdir(dirname(path), { recursive: true });
    await writeFile(path, bytes);
    report.pinned[file] = sha256(bytes);
    return bytes;
  })();
  pinned.set(file, loaded);
  return loaded;
}

await Promise.all(wpt.map(async proof => {
  const source = (await load(proof.file)).toString('utf8');
  const includes = [...source.matchAll(/^\/\/ META: script=(.+)$/gm)].map(([, entry]) => {
    const file = entry.trim();
    return file.startsWith('/') ? file.slice(1) : posix.normalize(posix.join(posix.dirname(proof.file), file));
  });
  await Promise.all(includes.map(load));
  proof.page = '/' + proof.file.replace(/\.js$/, '.html');
  pages.set(proof.page, '<!doctype html><meta charset="utf-8"><title>Fetch WPT</title>' +
    '<pre id="wpt-receipt"></pre><script src="/harness.js"></script>' +
    [...includes, proof.file].map(file => '<script src="/' + file + '"></script>').join('') +
    '<script>runStreamsTests();</script>');
}));

function reply(response, status, body, type = typeText, headers = {}) {
  response.writeHead(status, { 'Content-Type': type, 'Cache-Control': 'no-store', ...headers });
  response.end(body);
}

async function readBody(request) {
  const chunks = [];
  let length = 0;
  for await (const chunk of request) {
    length += chunk.length;
    assert.ok(length < 8192, 'Unexpected upload size in fixture');
    chunks.push(chunk);
  }
  return Buffer.concat(chunks);
}

let wire;
function initWire() {
  wire = {
    transcript: [], first: Promise.withResolvers(), abortData: Promise.withResolvers(),
    reset: Promise.withResolvers(), counts: new Map(),
  };
}
initWire();

async function serve(request, response) {
  const url = new URL(request.url, 'http://127.0.0.1');
  const path = url.pathname;
  if (path === '/fixture') return reply(response, 200, fixture, typeHtml);
  if (path === '/harness.js') return reply(response, 200, harness, typeScript);
  if (pages.has(path)) return reply(response, 200, pages.get(path), typeHtml);
  if (pinned.has(path.slice(1))) return reply(response, 200, await pinned.get(path.slice(1)), typeScript);
  const active = wire;
  if (path.startsWith('/wire/') || request.method !== 'GET') {
    active.transcript.push([request.method, path, request.httpVersion]);
  }
  if (path === '/wire/buffered') return reply(response, 200, await readBody(request));
  if (path === '/wire/first') return reply(response, 200, await active.first.promise);
  if (path === '/wire/abort-data') return reply(response, 200, await active.abortData.promise);
  if (path === '/wire/reset') return reply(response, 200, await active.reset.promise);
  if (path === '/wire/stream' && request.httpVersion === '2.0') {
    let length = 0;
    const firstBytes = [];
    response.writeHead(200, { 'Content-Type': typeText });
    response.flushHeaders();
    for await (const chunk of request) {
      length += chunk.length;
      assert.ok(length <= 6);
      if (length <= 3) {
        firstBytes.push(...chunk);
        if (length === 3) {
          active.transcript.push(['first data', firstBytes]);
          active.first.resolve(firstBytes.join(','));
        }
      }
      response.write([...chunk].map(byte => String.fromCharCode(65 + byte)).join(''));
    }
    active.transcript.push(['end upload', length]);
    return response.end();
  }
  if (path === '/wire/abort-stream' && request.httpVersion === '2.0') {
    request.on('error', () => {});
    request.stream.once('aborted', () => {
      active.transcript.push(['stream reset', request.stream.rstCode]);
      active.reset.resolve(String(request.stream.rstCode));
    });
    request.once('data', chunk => {
      active.transcript.push(['abort data', [...chunk]]);
      active.abortData.resolve([...chunk].join(','));
    });
    return;
  }
  if (path.endsWith('/echo-content.h2.py')) {
    response.writeHead(200, { 'Content-Type': typeText });
    response.flushHeaders();
    request.on('error', () => response.destroy());
    response.on('error', () => request.destroy());
    return request.pipe(response);
  }
  if (path.endsWith('/echo-content.py')) {
    return reply(response, 200, await readBody(request), typeText, {
      'X-Request-Method': request.method,
      'X-Request-Content-Length': request.headers['content-length'] ?? 'NO',
      'X-Request-Content-Type': request.headers['content-type'] ?? 'NO',
    });
  }
  if (path.endsWith('/authentication.py')) {
    await readBody(request);
    return reply(response, 401, 'Please log in', typeText,
      { 'WWW-Authenticate': 'Basic realm="' + url.searchParams.get('realm') + '"' });
  }
  if (path.endsWith('/network-partition-key.py')) {
    await readBody(request);
    const id = url.searchParams.get('uuid');
    const previous = active.counts.get(id) ?? { attempts: 0, connections: new Set() };
    previous.attempts++;
    previous.connections.add(request.stream.session);
    active.counts.set(id, previous);
    return reply(response, 421, 'ok. Request was sent ' + previous.attempts +
      ' times. ' + previous.connections.size + ' connections were created.');
  }
  return reply(response, 404, 'Unknown fixture path');
}

const errors = (name, error) => {
  const detail = String(error).slice(0, 2200);
  report.errors.push({ name, error: detail });
  console.error(name + ' ' + detail);
};
const handler = (request, response) => serve(request, response).catch(error => {
  errors('Loopback server ' + request.url, error);
  if (response.headersSent) response.destroy();
  else reply(response, 500, 'Fixture server error');
});
const h1 = createServer(handler);
const h2 = createSecureServer({ key, cert, allowHTTP1: false }, handler);
const sessions = new Set();
h2.on('session', session => {
  sessions.add(session);
  session.once('close', () => sessions.delete(session));
  session.on('error', error => errors('HTTP/2 session', error));
});
await Promise.all([h1, h2].map(async server => {
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
}));
const origins = { h1: 'http://127.0.0.1:' + h1.address().port, h2: 'https://127.0.0.1:' + h2.address().port };

async function command(executable, args, label, json = true, env = process.env) {
  const child = spawn(executable, args, { windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'], env });
  let stdout = '', stderr = '', expired = false;
  child.stdout.setEncoding('utf8').on('data', chunk => stdout += chunk);
  child.stderr.setEncoding('utf8').on('data', chunk => stderr += chunk);
  const timer = setTimeout(() => { expired = true; child.kill(); }, 40000);
  try {
    const [code] = await once(child, 'close');
    assert.equal(expired, false, label + ' timed out');
    assert.equal(code, 0, label + ' failed ' + stderr.slice(0, 1000));
    return json ? JSON.parse(stdout) : stdout.trim();
  } finally {
    clearTimeout(timer);
    await Promise.all([
      writeFile(resolve(output, label + '.stdout.txt'), stdout),
      writeFile(resolve(output, label + '.stderr.txt'), stderr),
    ]);
  }
}

const entities = { amp: '&', lt: '<', gt: '>', quot: '"', '#39': "'" };
function receipt(dom, id) {
  const match = new RegExp('<pre\\b([^>]*\\bid="' + id + '"[^>]*)>([^<]*)</pre>').exec(dom);
  assert.ok(match?.[1].includes('data-complete="yes"'), id + ' did not complete');
  return JSON.parse(match[2].replace(/&(amp|lt|gt|quot|#39);/g, (_, key) => entities[key]));
}

async function attempt(label, entry, action) {
  initWire();
  try { await action(); }
  catch (error) { errors(label, error); }
  finally { entry.wire = wire.transcript; }
}

function verifyHttp1(entry, synchronicity) {
  const { canceledSynchronously, ...rest } = entry.receipt;
  assert.deepEqual(rest, {
    duplex: 'half', sameBody: true, usedBefore: false, usedAfter: true, lockedAfter: true,
    pulls: 0, error: 'TypeError', buffered: 'control', rejectedWithReason: true,
  });
  if (synchronicity !== undefined) assert.equal(canceledSynchronously, synchronicity);
  assert.deepEqual(wire.transcript, [['POST', '/wire/buffered', '1.1']]);
}

function verifyHttp2(entry) {
  assert.deepEqual(entry.receipt, {
    pulls: 2, first: '11,12,13', received: 'LMNOPQ', waitedForEof: true, firstAborted: '77',
    reset: '8', rejectedWithReason: true, canceledWithReason: true,
  });
  assert.deepEqual(wire.transcript.filter(event => event[0] === 'POST'),
    [['POST', '/wire/stream', '2.0'], ['POST', '/wire/abort-stream', '2.0']]);
  assert.deepEqual(wire.transcript.filter(event => event.length === 2),
    [['first data', [11, 12, 13]], ['end upload', 6], ['abort data', [77]], ['stream reset', 8]]);
}

function verifyWpt(entry, dom, proof) {
  entry.tests = receipt(dom, 'wpt-receipt');
  verifyTests(entry, proof);
}

function verifyTests(entry, proof) {
  assert.equal(entry.tests.length, proof.cases);
  assert.deepEqual(entry.tests.filter(test => test.status !== 'pass'), []);
}

async function rustDom(url, name, entry, duration) {
  const path = resolve(output, name + '.dom.html');
  entry.result = await command(binary, ['--headless', '--url', url, '--profile',
    resolve(output, 'profile-' + randomUUID()), '--dump-dom', path, '--run-for-ms', String(duration)], name);
  return readFile(path, 'utf8');
}

async function runRust() {
  const hash = report.rust.binarySha256 = sha256(await readFile(binary));
  report.rust.version = await command(binary, ['--version'], 'rust-version', false);
  const headless = report.rust.headless = {};
  await attempt('Rust HTTP/1.1 headless', headless, async () => {
    headless.receipt = receipt(await rustDom(origins.h1 + '/fixture', 'rust-h1-headless', headless, 1000), 'receipt');
    verifyHttp1(headless, true);
    assert.deepEqual(headless.result.javascript.diagnostics, []);
  });
  const native = report.rust.native = {};
  await attempt('Rust HTTP/1.1 native', native, async () => {
    await save('native-actions.json', [{ action: 'title', value: 'Fetch upload verified' }]);
    native.result = await command(binary, ['--url', origins.h1 + '/fixture',
      '--profile', resolve(output, 'profile-' + randomUUID()), '--smoke-window', resolve(output, 'rust-h1-native.png'),
      '--smoke-actions', resolve(output, 'native-actions.json')], 'rust-h1-native');
    assert.equal(native.result.window_smoke, true);
    assert.ok(native.result.frames_presented > 0);
    assert.deepEqual(native.result.javascript.diagnostics, []);
    assert.deepEqual(wire.transcript, [['POST', '/wire/buffered', '1.1']]);
  });
  report.rust.wpt = {};
  for (const proof of wpt.filter(entry => !entry.h2)) {
    const entry = report.rust.wpt[proof.file] = {};
    await attempt('Rust ' + proof.file, entry, async () => {
      verifyWpt(entry, await rustDom(origins.h1 + proof.page, 'rust-' + basename(proof.file), entry, 4200), proof);
      assert.deepEqual(entry.result.javascript.diagnostics, []);
    });
  }
  assert.equal(sha256(await readFile(binary)), hash, 'Browser executable changed during verification');
  if (engineTest) {
    const proof = wpt.find(item => item.h2);
    const entry = report.rust.injectedTlsWpt = {};
    await attempt('Rust original HTTP/2 WPT with test root', entry, async () => {
      entry.engineTestSha256 = sha256(await readFile(engineTest));
      entry.testCaSha256 = sha256(await readFile(resolve(certificates, 'ca.pem')));
      const stdout = await command(engineTest, [
        'network::upload::tests::original_http2_upload_wpt', '--exact', '--ignored', '--nocapture',
      ], 'rust-h2-wpt', false, { ...process.env, RUST_BROWSER_UPLOAD_WPT_ROOT: resolve(output, 'pinned'),
        RUST_BROWSER_UPLOAD_WPT_URL: origins.h2 + proof.page });
      const match = /UPLOAD_WPT_RECEIPT (\[.+\])\r?$/m.exec(stdout);
      assert.ok(match, 'The opt-in engine test did not emit its pinned WPT receipt');
      entry.tests = JSON.parse(match[1]);
      verifyTests(entry, proof);
    });
  }
}

async function capture(browser, url, selector, label, entry) {
  const scenario = label + '.scenario.json';
  await save(scenario, {
    browser, start_url: { url },
    actions: [{ step_id: 'receipt', action: 'wait_for', locator: { kind: 'css', selector },
      state: 'attached', timeout_ms: 9000 }],
    capture: { at_end: ['url', 'dom'], events: ['page-errors'] },
    command_timeout_ms: 23000,
  });
  const result = await command(process.execPath, [
    rea, 'capture-browser-scenario', resolve(output, scenario), '--format', 'json', '--full-output',
  ], label);
  assert.ok(result.ok, JSON.stringify(result.errors ?? []));
  const normalized = result.data.normalized_result;
  entry.evidence = { id: result.data.evidence_id, browser: normalized.browser };
  const step = normalized.steps.at(-1);
  assert.equal(step.status, 'completed', JSON.stringify(step.error));
  assert.equal(step.artifacts.dom.state, 'captured');
  const dom = step.artifacts.dom.value.text;
  await writeFile(resolve(output, label + '.dom.html'), dom);
  return dom;
}

async function runChrome() {
  const profile = resolve(output, 'chrome-upload-' + randomUUID());
  const child = spawn(chrome, [
    '--headless=new', '--no-first-run', '--remote-debugging-port=0', '--user-data-dir=' + profile,
    '--ignore-certificate-errors-spki-list=' + spki, 'about:blank',
  ], { windowsHide: true, stdio: ['ignore', 'ignore', 'ignore'] });
  const closed = once(child, 'close').then(([exitCode, signal]) => ({ exitCode, signal }));
  closed.catch(() => {});
  let endpoint;
  try {
    const portFile = resolve(profile, 'DevToolsActivePort');
    for (let count = 0; count < 80; count++) {
      assert.equal(child.exitCode, null, 'The owned Chrome process exited on launch');
      const port = (await readFile(portFile, 'utf8').catch(() => '')).split(/\r?\n/)[0];
      if (/^\d+$/.test(port)) {
        endpoint = 'http://127.0.0.1:' + port;
        break;
      }
      await delay(100);
    }
    assert.ok(endpoint, 'The owned Chrome process did not open its debugging endpoint');
    const targets = await fetch(endpoint + '/json/list', { signal: AbortSignal.timeout(4000) }).then(value => value.json());
    const tab = targets.find(target => target.type === 'page' && target.url === 'about:blank');
    assert.ok(tab, 'The owned Chrome profile did not open its blank tab');
    const browser = { mode: 'connect', cdp_endpoint: endpoint, target_id: tab.id };
    report.chrome.leafKeyFingerprint = spki;
    for (const protocol of ['h1', 'h2']) {
      const entry = report.chrome[protocol] = {};
      const selector = '#receipt[data-complete="yes"]';
      await attempt('Chrome ' + protocol + ' fixture', entry, async () => {
        entry.receipt = receipt(await capture(browser, origins[protocol] + '/fixture', selector, 'chrome-' + protocol, entry),
          'receipt');
        if (protocol === 'h1') verifyHttp1(entry);
        else verifyHttp2(entry);
      });
    }
    report.chrome.wpt = {};
    for (const proof of wpt) {
      const entry = report.chrome.wpt[proof.file] = {};
      const selector = '#wpt-receipt[data-complete="yes"]';
      await attempt('Chrome ' + proof.file, entry, async () => {
        const url = origins[proof.h2 ? 'h2' : 'h1'] + proof.page;
        verifyWpt(entry, await capture(browser, url, selector, 'chrome-' + basename(proof.file), entry), proof);
      });
    }
  } finally {
    let stoppedBy = 'browser-close';
    if (endpoint && child.exitCode === null) {
      try {
        const version = await fetch(endpoint + '/json/version', { signal: AbortSignal.timeout(2000) }).then(value => value.json());
        const socket = new WebSocket(version.webSocketDebuggerUrl);
        await once(socket, 'open', { signal: AbortSignal.timeout(2000) });
        socket.send(JSON.stringify({ id: 1, method: 'Browser.close' }));
        await Promise.race([closed, delay(2500, undefined, { ref: false })]);
        socket.close();
      } catch (error) {
        report.chrome.closeDiagnostic = String(error);
      }
    }
    if (child.exitCode === null && child.signalCode === null) {
      stoppedBy = 'terminated-owned-process';
      child.kill();
    }
    report.chrome.cleanup = { stoppedBy, ...await closed };
  }
}

try {
  if (mode !== 'chrome') await runRust().catch(error => errors('Rust executable', error));
  if (mode !== 'rust') await runChrome().catch(error => errors('Owned Chrome', error));
} finally {
  const closed = [h1, h2].map(server => {
    const done = once(server, 'close');
    server.close();
    return done;
  });
  h1.closeAllConnections();
  for (const session of sessions) session.destroy();
  await Promise.all(closed);
  await save('report.json', report);
}
if (report.errors.length) process.exitCode = 1;
