// Serve owned fixtures and verify the shipped browser through its normal entry points.
// The native window is driven only by the browser's internal smoke actions.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { once } from 'node:events';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import * as resources from './fixtures/navigation/resources.mjs';

const repo = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const defaultRea = 'C:/Users/admin/AppData/Local/npm-cache/_npx/da7caeae9db9c589/node_modules/rea-agents/scripts/rea.mjs';
const defaultChrome = 'C:/Program Files/Google/Chrome/Application/chrome.exe';
const args = process.argv.slice(2);
const flags = new Set();
const options = new Map();
const positional = [];
for (let index = 0; index < args.length; index++) {
  const arg = args[index];
  if (['--chrome-reference', '--chrome-reference-only', '--rust-headless-only'].includes(arg)) {
    flags.add(arg);
  } else if (['--chrome', '--rea', '--timeout-ms'].includes(arg)) {
    assert.ok(args[index + 1] && !args[index + 1].startsWith('--'), `${arg} requires a value`);
    options.set(arg, args[++index]);
  } else {
    assert.ok(!arg.startsWith('--'), `Unknown option ${arg}`);
    positional.push(arg);
  }
}
assert.ok(positional.length <= 2, 'Usage: node tools/verify-navigation.mjs [BROWSER.exe] [EVIDENCE_DIR] [--chrome-reference | --chrome-reference-only] [--rust-headless-only] [--chrome PATH] [--rea PATH] [--timeout-ms N]');
const binary = resolve(positional[0] ?? resolve(repo, 'target/debug/rust-browser.exe'));
const output = resolve(positional[1] ?? resolve(repo, 'target/navigation-verification'));
const chromeOnly = flags.has('--chrome-reference-only');
const withChrome = chromeOnly || flags.has('--chrome-reference');
const timeout = Number(options.get('--timeout-ms') ?? 50000);
assert.ok(Number.isInteger(timeout) && timeout >= 10000 && timeout <= 180000);
await mkdir(output, { recursive: true });

const fixture = await readFile(new URL('./fixtures/navigation/index.html', import.meta.url), 'utf8');
const digest = (algorithm, value, encoding = 'base64') => createHash(algorithm).update(value).digest(encoding);
const goodIntegrity = `sha256-${digest('sha256', 'wrong weaker digest')} sha384-${digest('sha384', resources.sriGood)}`;
const badIntegrity = `sha256-${digest('sha256', resources.sriBad)} sha512-${digest('sha512', 'wrong stronger digest')}`;
const crossIntegrity = `sha256-${digest('sha256', resources.sriCross)}`;

const expectedTrace = [
  'outer-start', 'written-inline', 'outer-end', 'outer-microtask', 'written-external',
  'classic-redirect', 'sri-good', 'sri-cross', 'parser-tail', 'ready-interactive',
  'defer-first', 'defer-second', 'module-shared', 'module-relative', 'module-root',
  'cross-module-leaf', 'cross-module-root', 'module-inline', 'dom-content-loaded',
  'async-script', 'ready-complete', 'window-load',
];
const expectedObservations = {
  prefixOnly: true,
  blockingStyle: 'rgb(7, 79, 36)',
  base: true,
  outerCurrentScript: true,
  outerState: 'loading',
  partialReference: 'A ',
  writeln: 'A ©\n',
  nestedCurrentScript: true,
  synchronousWrite: true,
  restoredCurrentScript: true,
  externalPending: true,
  writtenExternal: [true, true, 'loading'],
  classic: [true, true, 'loading'],
  sriGood: true,
  sriCross: true,
  tailState: ['loading', true],
  writeOrder: ['external-write', 'external-tail', 'queued-write', 'after'],
  deferFirst: ['interactive', true],
  deferSecond: ['interactive', true],
  moduleRoot: [1, 2, 17, 9, true, 'interactive', true],
  moduleInline: [2, 42, 'interactive', true],
  domContentLoaded: ['interactive', true, true],
  asyncScript: ['interactive', true],
  load: ['complete', true],
};
const expectedResourceEvents = Object.fromEntries([
  ['nested-external', 'load'], ['classic', 'load'], ['sri-good', 'load'], ['sri-bad', 'error'],
  ['cross-integrity', 'load'], ['mime-rejected', 'error'], ['defer-first', 'load'], ['defer-second', 'load'],
  ['async-gate', 'load'], ['module-root', 'load'], ['cors-rejected', 'error'],
].map(([id, kind]) => [id, [kind, true, true]]));

const resourceFiles = new Map([
  ['block.css', ['text/css; charset=utf-8', resources.stylesheet, 100]],
  ['written.js', ['text/javascript; charset=utf-8', resources.written, 70]],
  ['classic/main.js', ['text/javascript; charset=utf-8', resources.classic, 0]],
  ['sri-good.js', ['text/javascript; charset=utf-8', resources.sriGood, 0]],
  ['sri-bad.js', ['text/javascript; charset=utf-8', resources.sriBad, 0]],
  ['bad-mime.js', ['text/plain; charset=utf-8', resources.badMime, 0]],
  ['defer-first.js', ['text/javascript; charset=utf-8', resources.deferFirst, 110]],
  ['defer-second.js', ['text/javascript; charset=utf-8', resources.deferSecond, 0]],
  ['modules/shared.js', ['text/javascript; charset=utf-8', resources.sharedModule, 0]],
  ['modules/redirected/root.js', ['text/javascript; charset=utf-8', resources.rootModule, 0]],
  ['modules/redirected/relative.js', ['text/javascript; charset=utf-8', resources.relativeModule, 0]],
]);

const escapeText = text => text.replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;').replaceAll('\u00a0', '&nbsp;');
const escapeAttribute = text => text.replaceAll('&', '&amp;').replaceAll('"', '&quot;').replaceAll('\u00a0', '&nbsp;');

function expectedFixtureHtml(tree) {
  // The four pinned scripted cases contain only these HTML tags, attributes and
  // text. Interpret their expected tree dump without parsing the test input.
  const root = { children: [] };
  const levels = [root];
  for (const line of tree.trimEnd().split('\n')) {
    const field = /^\| ( *)(.*)$/.exec(line);
    assert.ok(field && field[1].length % 2 === 0, `Unknown tree line ${line}`);
    const depth = field[1].length / 2;
    const parent = levels[depth];
    assert.ok(parent, `Missing parent for ${line}`);
    const content = field[2];
    const tag = /^<([a-z]+)>$/.exec(content);
    const attr = /^([a-z-]+)="(.*)"$/.exec(content);
    if (tag) {
      assert.ok(['html', 'head', 'body', 'p', 'b', 'font', 'script'].includes(tag[1]), `Unsupported fixture tag ${tag[1]}`);
      const node = { tag: tag[1], attrs: [], children: [] };
      parent.children.push(node);
      levels.length = depth + 1;
      levels.push(node);
    } else if (attr) {
      assert.ok(parent.attrs, `Attribute without element ${line}`);
      parent.attrs.push([attr[1], attr[2]]);
    } else {
      assert.ok(content.startsWith('"') && content.endsWith('"'), `Unknown fixture node ${line}`);
      parent.children.push(content.slice(1, -1));
    }
  }
  function serialize(node, raw = false) {
    if (typeof node === 'string') return raw ? node : escapeText(node);
    const body = node.children.map(child => serialize(child, node.tag === 'script')).join('');
    if (!node.tag) return body;
    const attrs = node.attrs.map(([name, value]) => ` ${name}="${escapeAttribute(value)}"`).join('');
    return `<${node.tag}${attrs}>${body}</${node.tag}>`;
  }
  return serialize(root);
}

async function loadScriptedFixtures() {
  const directory = resolve(repo, 'crates/engine/data/html5lib');
  const hashes = await readFile(resolve(directory, 'SHA256SUMS'), 'utf8');
  const result = new Map();
  for (const file of ['adoption01.dat', 'ark.dat', 'webkit01.dat']) {
    const relative = `scripted/${file}`;
    const bytes = await readFile(resolve(directory, relative));
    const sha256 = digest('sha256', bytes, 'hex');
    assert.ok(hashes.split(/\r?\n/).includes(`${sha256}  ${relative}`), `${relative} fixture hash`);
    const records = `\n${bytes.toString('utf8')}`.split('\n#data\n').slice(1);
    for (const [index, record] of records.entries()) {
      const boundary = record.indexOf('\n#errors');
      const treeMarker = '\n#document\n';
      const tree = record.indexOf(treeMarker, boundary);
      assert.ok(boundary >= 0 && tree > boundary && record.slice(boundary, tree).includes('\n#script-on'));
      const source = Buffer.from(record.slice(0, boundary), 'utf8');
      const id = `${file.slice(0, -4)}-${index + 1}`;
      result.set(id, {
        file: relative,
        sha256,
        source,
        sourceSha256: digest('sha256', source, 'hex'),
        expected: expectedFixtureHtml(record.slice(tree + treeMarker.length)),
      });
    }
  }
  assert.equal(result.size, 4, 'Pinned scripted case count');
  return result;
}

const scripted = await loadScriptedFixtures();
const requests = [];
const receipts = new Map();
const streams = new Map();
const asyncGates = new Map();
const fixtureSources = new Map();
let pageOrigin = '';
let crossOrigin = '';

function reply(response, status, type, body, headers = {}) {
  response.writeHead(status, { 'Content-Type': type, 'Cache-Control': 'no-store', ...headers });
  response.end(body);
}
const html = 'text/html; charset=utf-8';
const javascript = 'text/javascript; charset=utf-8';
const plain = 'text/plain; charset=utf-8';
function redirect(response, url) {
  reply(response, 302, plain, '', { Location: url });
}
function gateFor(run) {
  if (!asyncGates.has(run)) {
    asyncGates.set(run, { released: false, deadline: false, waiter: null });
  }
  return asyncGates.get(run);
}
function recordRequest(origin, request, response) {
  const entry = { origin, method: request.method, path: request.url, originHeader: request.headers.origin ?? null, status: null };
  requests.push(entry);
  response.once('finish', () => { entry.status = response.statusCode; });
}
function renderFixture(run) {
  const base = `${pageOrigin}/cases/${run}/assets/`;
  const cross = `${crossOrigin}/cross/${run}/`;
  return fixture
    .replace('__BASE_JSON__', JSON.stringify(base))
    .replace('__RELEASE_JSON__', JSON.stringify(`${pageOrigin}/release/${run}`))
    .replace('__RESULT_JSON__', JSON.stringify(`${pageOrigin}/result/${run}`))
    .replace('__SRI_GOOD__', goodIntegrity)
    .replace('__SRI_BAD__', badIntegrity)
    .replace('__SRI_CROSS__', crossIntegrity)
    .replace('__CROSS_SRI__', `${cross}sri.js`)
    .replace('__CROSS_DENIED__', `${cross}denied.js`)
    .replace('__CROSS_REDIRECT_JSON__', JSON.stringify(`${cross}redirect.js`));
}

const runs = '(headless|native|chrome)';
const entryPattern = new RegExp(`^/entry/${runs}$`);
const casePattern = new RegExp(`^/cases/${runs}/(.*)$`);
const modePattern = new RegExp(`^/(release|result)/${runs}$`);
const scriptedPattern = new RegExp(`^/scripted/${runs}/([a-z0-9-]+)\\.html$`);
const crossPattern = new RegExp(`^/cross/${runs}/(.*)$`);
const streamPattern = new RegExp(`^/stream/${runs}/(index\\.html|early\\.js|result)$`);
const crabBytes = Buffer.from('🦀');
const streamChunks = [
  Buffer.from(resources.streamPrefix),
  Buffer.concat([Buffer.from(resources.streamTailBeforeCrab), crabBytes.subarray(0, 2)]),
  Buffer.concat([crabBytes.subarray(2), Buffer.from(resources.streamTailAfterCrab)]),
];

async function readReceipt(request) {
  const body = [];
  let bytes = 0;
  for await (const chunk of request) {
    bytes += chunk.length;
    if (bytes > 65536) throw new Error('Receipt exceeded byte limit');
    body.push(chunk);
  }
  return JSON.parse(Buffer.concat(body).toString('utf8'));
}

async function serveStream(run, route, request, response) {
  if (route === 'index.html' && request.method === 'GET') {
    if (streams.has(run)) {
      reply(response, 409, plain, 'Repeated stream navigation');
      return;
    }
    const state = { events: ['prefix'], releasedBy: null, receipt: null, release: null };
    streams.set(run, state);
    response.writeHead(200, { 'Content-Type': html, 'Cache-Control': 'no-store' });
    response.socket.setNoDelay(true);
    response.flushHeaders();
    response.write(streamChunks[0]);
    let tailTimer;
    const deadline = setTimeout(() => state.release('deadline'), 4000);
    state.release = reason => {
      if (state.releasedBy || response.destroyed) return;
      clearTimeout(deadline);
      state.releasedBy = reason;
      state.events.push('tail-partial-utf8');
      response.write(streamChunks[1]);
      tailTimer = setTimeout(() => {
        if (!response.destroyed) {
          state.events.push('tail-final-utf8');
          response.end(streamChunks[2]);
        }
      }, 80);
    };
    response.once('close', () => { clearTimeout(deadline); clearTimeout(tailTimer); });
    return;
  }
  const state = streams.get(run);
  if (!state) {
    reply(response, 400, plain, 'Stream request before navigation');
  } else if (route === 'early.js' && request.method === 'GET') {
    state.events.push('early-script-request');
    state.release('early-script-request');
    reply(response, 200, javascript, resources.streamScript);
  } else if (route === 'result' && request.method === 'POST') {
    try {
      if (state.receipt) throw new Error('Duplicate streaming receipt');
      state.receipt = await readReceipt(request);
      state.events.push('receipt');
      reply(response, 200, plain, 'OK');
    } catch (error) {
      reply(response, 400, plain, String(error));
    }
  } else {
    reply(response, 405, plain, 'Unexpected streaming request');
  }
}

const page = createServer(async (request, response) => {
  recordRequest('page', request, response);
  const path = new URL(request.url, pageOrigin).pathname;
  const entry = entryPattern.exec(path);
  const fixtureCase = casePattern.exec(path);
  const scriptedCase = scriptedPattern.exec(path);
  const report = modePattern.exec(path);
  const streamCase = streamPattern.exec(path);
  if (streamCase) {
    await serveStream(streamCase[1], streamCase[2], request, response);
    return;
  }
  if (request.method === 'POST' && report) {
    const [, operation, run] = report;
    if (operation === 'release') {
      const gate = gateFor(run);
      gate.released = true;
      gate.waiter?.();
      reply(response, 204, plain, '');
      return;
    }
    try {
      if (receipts.has(run)) throw new Error('Duplicate receipt');
      receipts.set(run, await readReceipt(request));
      reply(response, 200, plain, 'OK');
    } catch (error) {
      reply(response, 400, plain, String(error));
    }
    return;
  }
  if (request.method !== 'GET') {
    reply(response, 405, plain, 'Unexpected method');
  } else if (entry) {
    redirect(response, `../cases/${entry[1]}/document/index.html`);
  } else if (scriptedCase && scripted.has(scriptedCase[2])) {
    reply(response, 200, html, scripted.get(scriptedCase[2]).source);
  } else if (fixtureCase) {
    const [, run, route] = fixtureCase;
    if (route === 'document/index.html') {
      const source = renderFixture(run);
      fixtureSources.set(run, source);
      reply(response, 200, html, source);
    } else if (route === 'assets/classic-redirect.js') {
      redirect(response, './classic/main.js');
    } else if (route === 'assets/module-redirect.js') {
      redirect(response, './modules/redirected/root.js');
    } else if (route === 'assets/async-gate.js') {
      const gate = gateFor(run);
      if (gate.released) {
        reply(response, 200, javascript, resources.asyncGate);
        return;
      }
      const timer = setTimeout(() => {
        gate.deadline = true;
        gate.waiter?.();
      }, 5000);
      gate.waiter = () => {
        clearTimeout(timer);
        gate.waiter = null;
        if (!response.destroyed) reply(response, 200, javascript, resources.asyncGate);
      };
      response.once('close', () => { clearTimeout(timer); gate.waiter = null; });
    } else if (route.startsWith('assets/') && resourceFiles.has(route.slice(7))) {
      const [type, source, delay] = resourceFiles.get(route.slice(7));
      const headers = route.endsWith('/bad-mime.js') ? { 'X-Content-Type-Options': 'nosniff' } : {};
      if (delay) await new Promise(done => setTimeout(done, delay));
      if (!response.destroyed) reply(response, 200, type, source, headers);
    } else {
      reply(response, 404, plain, `Unknown navigation resource ${path}`);
    }
  } else {
    reply(response, 404, plain, 'Not found');
  }
});

const cross = createServer((request, response) => {
  recordRequest('cross', request, response);
  const path = new URL(request.url, crossOrigin).pathname;
  const match = crossPattern.exec(path);
  if (request.method !== 'GET' || !match) {
    reply(response, 404, plain, 'Unknown cross-origin resource');
    return;
  }
  const cors = { 'Access-Control-Allow-Origin': pageOrigin };
  switch (match[2]) {
    case 'sri.js': reply(response, 200, javascript, resources.sriCross, cors); break;
    case 'denied.js': reply(response, 200, javascript, resources.corsDenied); break;
    case 'redirect.js': reply(response, 302, plain, '', { ...cors, Location: './bundle/root.js' }); break;
    case 'bundle/root.js': reply(response, 200, javascript, resources.crossEntry, cors); break;
    case 'bundle/leaf.js': reply(response, 200, javascript, resources.crossLeaf, cors); break;
    default: reply(response, 404, plain, `Unknown cross-origin resource ${path}`);
  }
});

async function listen(server) {
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  return `http://127.0.0.1:${server.address().port}`;
}
async function close(server) {
  if (!server.listening) return;
  const closing = new Promise((done, reject) => server.close(error => error ? reject(error) : done()));
  server.closeAllConnections();
  await closing;
}

async function putJson(name, value) {
  await writeFile(resolve(output, name), JSON.stringify(value, null, 2) + '\n');
}
async function pngEvidence(path) {
  const bytes = await readFile(path);
  assert.ok(bytes.length > 24 && bytes.subarray(0, 8).equals(Buffer.from([137, 80, 78, 71, 13, 10, 26, 10])), `Invalid PNG ${path}`);
  assert.equal(bytes.subarray(12, 16).toString('ascii'), 'IHDR');
  const width = bytes.readUInt32BE(16);
  const height = bytes.readUInt32BE(20);
  assert.ok(width > 0 && height > 0, `Empty PNG ${path}`);
  return { path, width, height, sha256: digest('sha256', bytes, 'hex') };
}
async function runProcess(executable, argv, stem, limit = timeout, format = 'json') {
  await putJson(`${stem}-invocation.json`, { executable, argv, timeoutMs: limit });
  let stdout = '';
  let stderr = '';
  let stopped = '';
  let timer;
  try {
    const child = spawn(executable, argv, { windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
    const stop = reason => { if (!stopped) { stopped = reason; child.kill(); } };
    timer = setTimeout(() => stop('deadline exceeded'), limit);
    child.stdout.setEncoding('utf8').on('data', chunk => {
      if (!stopped && stdout.length + chunk.length <= 24 * 1024 * 1024) stdout += chunk;
      else stop('stdout limit exceeded');
    });
    child.stderr.setEncoding('utf8').on('data', chunk => {
      if (!stopped && stderr.length + chunk.length <= 4 * 1024 * 1024) stderr += chunk;
      else stop('stderr limit exceeded');
    });
    const [code, signal] = await once(child, 'close');
    assert.equal(stopped, '', `${stem} ${stopped}`);
    assert.equal(code, 0, `${stem} exit ${code ?? signal}: ${stderr.slice(-2000)}`);
    return format === 'json' ? JSON.parse(stdout) : stdout.trim();
  } catch (error) {
    await putJson(`${stem}-error.json`, {
      message: String(error.message ?? error), code: error.code ?? null,
      errno: error.errno ?? null, syscall: error.syscall ?? null,
    });
    throw error;
  } finally {
    clearTimeout(timer);
    await writeFile(resolve(output, `${stem}-stdout.${format === 'json' ? 'json' : 'txt'}`), stdout);
    await writeFile(resolve(output, `${stem}-stderr.txt`), stderr);
  }
}

function verifyReceipt(run) {
  const receipt = receipts.get(run);
  assert.ok(receipt, `${run} load did not post its result`);
  assert.deepEqual(receipt.issues, [], `${run} page assertions`);
  assert.deepEqual(receipt.trace, expectedTrace, `${run} script and lifecycle order`);
  assert.deepEqual(receipt.observations, expectedObservations, `${run} page observations`);
  assert.deepEqual(receipt.resourceEvents, expectedResourceEvents, `${run} script element load/error events`);
  assert.ok(gateFor(run).released && !gateFor(run).deadline, `${run} async response was gated by DOMContentLoaded`);
  return receipt;
}

function verifyRequests(run) {
  const local = `/cases/${run}/assets/`;
  const foreign = `/cross/${run}/`;
  const required = [
    ['page', `/entry/${run}`], ['page', `/cases/${run}/document/index.html`],
    ...[...resourceFiles.keys(), 'async-gate.js', 'classic-redirect.js', 'module-redirect.js'].map(name => ['page', local + name]),
    ...['sri.js', 'denied.js', 'redirect.js', 'bundle/root.js', 'bundle/leaf.js'].map(name => ['cross', foreign + name]),
  ];
  for (const [origin, path] of required) {
    const matches = requests.filter(request => request.origin === origin && request.path === path);
    assert.equal(matches.length, 1, `${run} requests ${path} exactly once`);
    assert.equal(matches[0].method, 'GET', path);
    assert.equal(matches[0].status, path.startsWith('/entry/') || path.endsWith('redirect.js') ? 302 : 200, `${path} response status`);
    if (origin === 'cross') assert.equal(matches[0].originHeader, pageOrigin, `${path} CORS Origin`);
  }
  const release = requests.findIndex(request => request.method === 'POST' && request.path === `/release/${run}`);
  const result = requests.findIndex(request => request.method === 'POST' && request.path === `/result/${run}`);
  assert.ok(release >= 0 && result > release, `${run} reports load after DOMContentLoaded release`);
  assert.ok(!requests.some(request => request.path === `${local}relative.js` || request.path === `${foreign}leaf.js`), `${run} relative module imports use final redirect URLs`);
}

const report = {
  startedAt: new Date().toISOString(),
  ok: false,
  binary: chromeOnly ? null : binary,
  fixtureRevision: '9329e64694e7835d0dcff9811e22856ef6ad16f9',
  expected: { trace: expectedTrace, observations: expectedObservations, resourceEvents: expectedResourceEvents },
  failures: [],
  modes: {},
  screenshots: {},
  streaming: { chunkBytes: streamChunks.map(chunk => chunk.length), splitUtf8: streamChunks.slice(1).map((chunk, index) => (index === 0 ? chunk.subarray(-2) : chunk.subarray(0, 2)).toString('hex')), modes: {} },
  scripted: Object.fromEntries([...scripted].map(([id, f]) => [id, { file: f.file, sha256: f.sha256, sourceSha256: f.sourceSha256, matches: {} }])),
};
const ownProfile = stem => resolve(output, `qa-${stem}-${randomUUID()}`);
const entryUrl = run => `${pageOrigin}/entry/${run}`;
const finalUrl = run => `${pageOrigin}/cases/${run}/document/index.html`;
const scriptedUrl = (run, id) => `${pageOrigin}/scripted/${run}/${id}.html`;
const streamUrl = run => `${pageOrigin}/stream/${run}/index.html`;

function verifyStream(run) {
  const state = streams.get(run);
  assert.ok(state, `${run} HTTP stream opened`);
  assert.equal(state.releasedBy, 'early-script-request', `${run} external script requested before main EOF`);
  assert.deepEqual(state.events, ['prefix', 'early-script-request', 'tail-partial-utf8', 'tail-final-utf8', 'receipt']);
  assert.deepEqual(state.receipt, {
    prefix: true,
    tailBeforeScript: false,
    scriptState: 'loading',
    scriptIdentity: true,
    readyState: 'interactive',
    tail: 'Zażółć 🦀 café',
  }, `${run} streamed DOM visibility and UTF-8 decoding`);
  const actions = requests.filter(request => request.path.startsWith(`/stream/${run}/`));
  assert.deepEqual(actions.map(({ method, path, status }) => [method, path.split('/').at(-1), status]), [
    ['GET', 'index.html', 200], ['GET', 'early.js', 200], ['POST', 'result', 200],
  ]);
}

function verifyResourceDiagnostics(result) {
  assert.deepEqual(result.javascript.diagnostics.slice().sort(), [
    'CORS response has no Access-Control-Allow-Origin',
    'Classic script blocked by MIME policy: "text/plain"',
    'Subresource integrity digest mismatch',
  ]);
}

async function attempt(name, check) {
  try {
    await check();
    return true;
  } catch (error) {
    report.failures.push({ name, error: String(error.stack ?? error) });
    return false;
  }
}

async function verifyRust() {
  report.binarySha256 = digest('sha256', await readFile(binary), 'hex');
  report.binaryVersion = await runProcess(binary, ['--version'], 'rust-version', timeout, 'text');
  assert.ok(report.binaryVersion, 'Rust Browser version check returned no version');
  const headlessOk = await attempt('Rust headless navigation', async () => {
    const domPath = resolve(output, 'rust-headless.html');
    const screenshot = resolve(output, 'rust-headless.png');
    const headless = await runProcess(binary, [
      '--headless', '--url', entryUrl('headless'), '--profile', ownProfile('headless'),
      '--run-for-ms', '3000', '--dump-dom', domPath,
      '--screenshot', screenshot,
    ], 'rust-headless');
    report.modes.headless = headless;
    assert.equal(headless.url, finalUrl('headless'));
    assert.equal(headless.title, 'Navigation pipeline OK');
    assert.equal(headless.layout_limited, false);
    assert.equal(headless.paint_limited, false);
    verifyResourceDiagnostics(headless);
    assert.match(await readFile(domPath, 'utf8'), /data-done="yes"/);
    report.screenshots.headless = await pngEvidence(screenshot);
    verifyReceipt('headless');
    verifyRequests('headless');
  });

  if (!flags.has('--rust-headless-only')) {
    await attempt('Rust native navigation', async () => {
      const screenshot = resolve(output, 'rust-native.png');
      await putJson('native-actions.json', [{ action: 'title', value: 'Navigation pipeline OK' }]);
      const native = await runProcess(binary, [
        '--url', entryUrl('native'), '--profile', ownProfile('native'),
        '--smoke-window', screenshot,
        '--smoke-actions', resolve(output, 'native-actions.json'),
      ], 'rust-native');
      report.modes.native = native;
      assert.equal(native.url, finalUrl('native'));
      assert.equal(native.title, 'Navigation pipeline OK');
      assert.equal(native.window_smoke, true);
      assert.ok(native.frames_presented > 0);
      assert.equal(native.layout_limited, false);
      assert.equal(native.paint_limited, false);
      verifyResourceDiagnostics(native);
      report.screenshots.native = await pngEvidence(screenshot);
      const receipt = verifyReceipt('native');
      if (headlessOk) assert.deepEqual(receipt, receipts.get('headless'));
      verifyRequests('native');
    });
  }

  await attempt('Rust headless HTTP streaming', async () => {
    const domPath = resolve(output, 'rust-stream-headless.html');
    const result = await runProcess(binary, [
      '--headless', '--url', streamUrl('headless'), '--profile', ownProfile('stream-headless'),
      '--run-for-ms', '2000', '--dump-dom', domPath,
    ], 'rust-stream-headless');
    report.streaming.modes.headless = result;
    assert.equal(result.url, streamUrl('headless'));
    assert.equal(result.title, 'HTTP streaming OK');
    assert.deepEqual(result.javascript.diagnostics, []);
    assert.match(await readFile(domPath, 'utf8'), /Zażółć 🦀 café/u);
    verifyStream('headless');
  });
  if (!flags.has('--rust-headless-only')) {
    await attempt('Rust native HTTP streaming', async () => {
      const screenshot = resolve(output, 'rust-stream-native.png');
      const actions = 'native-stream-actions.json';
      await putJson(actions, [{ action: 'title', value: 'HTTP streaming OK' }]);
      const result = await runProcess(binary, [
        '--url', streamUrl('native'), '--profile', ownProfile('stream-native'),
        '--smoke-window', screenshot, '--smoke-actions', resolve(output, actions),
      ], 'rust-stream-native');
      report.streaming.modes.native = result;
      assert.equal(result.url, streamUrl('native'));
      assert.equal(result.title, 'HTTP streaming OK');
      assert.equal(result.window_smoke, true);
      assert.ok(result.frames_presented > 0);
      assert.deepEqual(result.javascript.diagnostics, []);
      report.screenshots.streamNative = await pngEvidence(screenshot);
      verifyStream('native');
    });
  }

  for (const [id, f] of scripted) {
    report.scripted[id].matches.rust = await attempt(`Rust scripted ${id}`, async () => {
      const stem = `rust-scripted-${id}`;
      const domPath = resolve(output, `${stem}.html`);
      const result = await runProcess(binary, [
        '--headless', '--url', scriptedUrl('headless', id), '--profile', ownProfile(id),
        '--run-for-ms', '150', '--dump-dom', domPath,
      ], stem);
      report.scripted[id].rust = result;
      assert.equal(result.url, scriptedUrl('headless', id));
      assert.deepEqual(result.javascript.diagnostics, [], `${id} JavaScript diagnostics`);
      assert.equal(await readFile(domPath, 'utf8'), f.expected, `${id} upstream expected tree`);
    });
  }
}

async function captureChrome(url, selector, stem) {
  const scenario = {
    browser: { mode: 'launch', executable_path: options.get('--chrome') ?? defaultChrome, headless: true },
    start_url: { url },
    actions: [{ step_id: 'document-ready', action: 'wait_for', timeout_ms: 12000, locator: { kind: 'css', selector }, state: 'attached' }],
    capture: { at_end: ['url', 'dom'], events: ['page-errors', 'network'] },
    command_timeout_ms: 40000,
  };
  const scenarioPath = resolve(output, `${stem}-scenario.json`);
  await putJson(`${stem}-scenario.json`, scenario);
  const capture = await runProcess(process.execPath, [
    resolve(options.get('--rea') ?? defaultRea), 'capture-browser-scenario', scenarioPath, '--format', 'json', '--full-output',
  ], stem, Math.max(timeout, 65000));
  assert.equal(capture.ok, true, `${stem} REA result`);
  const normalized = capture.data.normalized_result;
  assert.equal(normalized.browser.process_ownership, 'provider-owned', `${stem} Chrome ownership`);
  assert.equal(normalized.browser.cleanup, 'terminated-owned-process', `${stem} Chrome cleanup`);
  assert.ok(normalized.steps.every(step => step.status === 'completed'), `${stem} completed steps`);
  const last = normalized.steps.at(-1);
  const dom = last.artifacts.dom;
  assert.equal(dom.state, 'captured', `${stem} DOM captured`);
  await writeFile(resolve(output, `${stem}.html`), dom.value.text);
  return {
    dom: dom.value.text,
    url: last.after_url.url,
    evidence: {
      evidenceId: capture.data.evidence_id,
      browser: normalized.browser,
      completeness: normalized.completeness,
      limitations: normalized.limitations,
      steps: normalized.steps.map(step => ({ id: step.step_id, status: step.status })),
    },
  };
}

async function verifyChrome() {
  await attempt('Chrome navigation', async () => {
    const main = await captureChrome(entryUrl('chrome'), '#result[data-done="yes"]', 'chrome-navigation');
    report.modes.chrome = main.evidence;
    assert.equal(main.url, finalUrl('chrome'));
    assert.match(main.dom, /<title>Navigation pipeline OK<\/title>/);
    const receipt = verifyReceipt('chrome');
    verifyRequests('chrome');
    if (receipts.has('headless')) assert.deepEqual(receipt, receipts.get('headless'));
  });
  await attempt('Chrome HTTP streaming', async () => {
    const capture = await captureChrome(streamUrl('chrome'), '#stream-result[data-done="yes"]', 'chrome-stream');
    report.streaming.modes.chrome = capture.evidence;
    assert.equal(capture.url, streamUrl('chrome'));
    assert.match(capture.dom, /Zażółć 🦀 café/u);
    verifyStream('chrome');
  });
  for (const [id, f] of scripted) {
    report.scripted[id].matches.chrome = await attempt(`Chrome scripted ${id}`, async () => {
      const url = scriptedUrl('chrome', id);
      const capture = await captureChrome(url, 'body', `chrome-scripted-${id}`);
      report.scripted[id].chrome = capture.evidence;
      assert.equal(capture.url, url);
      assert.equal(capture.dom, f.expected, `${id} Chrome vs upstream expected tree`);
    });
  }
}

try {
  pageOrigin = await listen(page);
  crossOrigin = await listen(cross);
  report.origins = { page: pageOrigin, cross: crossOrigin };
  await writeFile(resolve(output, 'streaming-source.html'), Buffer.concat(streamChunks));
  for (const [index, chunk] of streamChunks.entries()) {
    await writeFile(resolve(output, `streaming-chunk-${index + 1}.bin`), chunk);
  }
  // Save byte-exact inputs and full expected trees for review.
  for (const [id, f] of scripted) {
    await writeFile(resolve(output, `scripted-${id}-source.html`), f.source);
    await writeFile(resolve(output, `scripted-${id}-expected.html`), f.expected);
  }
  if (!chromeOnly) await attempt('Rust executable verification', verifyRust);
  if (withChrome) await verifyChrome();
  report.ok = report.failures.length === 0;
  if (!report.ok) process.exitCode = 1;
} catch (error) {
  report.error = String(error.stack ?? error);
  process.exitCode = 1;
} finally {
  try {
    for (const [run, source] of fixtureSources) await writeFile(resolve(output, `${run}-source.html`), source);
    report.requests = requests;
    report.receipts = Object.fromEntries(receipts);
    report.asyncGates = Object.fromEntries([...asyncGates].map(([run, gate]) => [run, { released: gate.released, deadline: gate.deadline }]));
    report.streaming.transport = Object.fromEntries([...streams].map(([run, state]) => [run, { events: state.events, releasedBy: state.releasedBy, receipt: state.receipt }]));
    await putJson('report.json', report);
  } finally {
    await Promise.all([close(page), close(cross)]);
  }
}

process.stdout.write(JSON.stringify({
  ok: report.ok,
  report: resolve(output, 'report.json'),
  modes: Object.keys(report.modes),
  streaming: Object.keys(report.streaming.modes),
  scripted: Object.fromEntries(Object.entries(report.scripted).map(([id, outcome]) => [id, outcome.matches])),
  failures: report.failures.map(failure => failure.name),
  error: report.error,
}) + '\n');
