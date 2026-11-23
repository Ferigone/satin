import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { once } from 'node:events';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const repo = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const fixtures = resolve(repo, 'tools/fixtures/dynamic-scripts');
const usage = 'Usage: node tools/verify-dynamic-scripts.mjs [BROWSER.exe] [OUTPUT] [--chrome-reference | --chrome-reference-only] [--rea PATH] [--chrome PATH]';
const args = process.argv.slice(2);
const positional = [];
const options = new Map();
for (let i = 0; i < args.length; i++) {
  const arg = args[i];
  if (arg === '--chrome-reference' || arg === '--chrome-reference-only') {
    options.set(arg, true);
  } else if (arg === '--rea' || arg === '--chrome') {
    assert.ok(args[i + 1] && !args[i + 1].startsWith('--'), usage);
    options.set(arg, args[++i]);
  } else {
    assert.ok(!arg.startsWith('--'), usage);
    positional.push(arg);
  }
}
assert.ok(positional.length <= 2, usage);
const binary = resolve(positional[0] ?? resolve(repo, 'target/release/rust-browser.exe'));
const output = resolve(positional[1] ?? resolve(repo, 'target/dynamic-script-verification'));
const chromeOnly = options.has('--chrome-reference-only');
const withChrome = chromeOnly || options.has('--chrome-reference');
const rea = resolve(options.get('--rea') ?? 'C:/Users/admin/AppData/Local/npm-cache/_npx/da7caeae9db9c589/node_modules/rea-agents/scripts/rea.mjs');
const chrome = resolve(options.get('--chrome') ?? 'C:/Program Files/Google/Chrome/Application/chrome.exe');
await mkdir(output, { recursive: true });

const sha256 = bytes => createHash('sha256').update(bytes).digest('hex');
const put = (path, value) => writeFile(resolve(output, path), JSON.stringify(value, null, 2) + '\n');
const html = 'text/html; charset=utf-8';
const js = 'text/javascript; charset=utf-8';
const wptRevision = 'fa649586f35119af9032312ef21e300e41d1c3b6';
const pins = [
  ['change-src-attr-prepare-a-script.html', 'b04489d86936d9bcdae32b73febd4be00c2b7afb723605e3ac36bffa8b772f6f', 1],
  ['changing-src.html', 'ac754610c1ef20664b2c4c9524ea1a319fa5603702226a9114b86e2a3189aa40', 1],
  ['changing-src.js', 'cc9e9d7db27de1ed90d6e459350a2fd9e5d5c2cb6d0531630ed52c1f977eb1bb'],
  ['dynamically-created-defer-script.html', 'dda0b49255a5535fb09e31012ffdfab93a357e879cf6323876c0721800f54433', 1],
  ['resources/dynamically-created-defer-script.js', '413b63185cc527bc8ca488f2a058dc354e09042f2c4e4068aa183db22ca6d8ae'],
  ['emptyish-script-elements.html', '96481ad6ccb64ae9764fea694e66f7401175ce10ce70a32e9a4fe2b6accf7608', 7],
  ['nomodule-reflect.html', '48e692be6f753ed9193aad6b209bc430ee76c103e52b8e4a983d1d5e6ab34669', 10],
  ['nomodule-set-on-inline-classic-scripts.html', '90d51cde13669844333aa93ec6dd3a8ba308728a470c425a8673fc630a58848b', 4],
  ['resources/flag-setter.js', '634739abe6a47e3d2b7949d77b5c991a936655a9b0def0c4477d33f3fd09f27a'],
  ['script-supports.html', '73b10554ad5f57251437a6d76473880ccbf68842eb9f0252a53eb730e36b5353', 5],
  ['src-empty-or-invalid-error-event-queued.html', 'bb4aaa98193dc5ff55263426bda56d1f5e37ba9369042325af37eb0c24cf7c55', 4],
  ['LICENSE.md', '5fac07febb0e2a97fb0d7b0def149ec08b642e1ba4b9c345283ab1cbd2af6570'],
];
const pinned = new Map(await Promise.all(pins.map(async ([name, hash]) => {
  const bytes = await readFile(resolve(fixtures, 'wpt', name));
  assert.equal(sha256(bytes), hash, name);
  return [name, bytes];
})));
const wptCases = pins.filter(([, , count]) => count !== undefined);
const fixture = await readFile(resolve(fixtures, 'index.html'), 'utf8');
const moduleFixture = await readFile(resolve(fixtures, 'module-lifecycle.html'));
const harness = await readFile(resolve(fixtures, 'testharness.js'));
const expected = {
  inline: ['inline', 'empty-parser-script', 'parsed-template', 'first', 'after-fragment', 'second', 'late-text'],
  life: ['dcl:interactive', 'boot:interactive', 'boot-load', 'load:complete', 'click:complete'],
  ordered: ['fast', 'fast-load', 'first-error', 'second', 'second-load'],
  modules: ['shared', 'redirect-leaf', 'root:42:true', 'root-load', 'inline:19:true', 'cross-leaf', 'cross:11:true'],
  imports: ['tla-start', 'tla-finish', 'namespace', 'nested-import'],
  checks: Object.fromEntries(['atomic', 'restored', 'emptyParserAsync', 'boot', 'clickCurrent', 'async', 'srcReflection', 'writeIgnored', 'namespaceIdentity', 'importReferrer', 'importRejection'].map(name => [name, true])),
  outcomes: Object.fromEntries([['fast', 'load'], ['first', 'error'], ['second', 'load'], ['cross', 'load'], ['cors', 'error'], ['sri', 'error']].map(([name, type]) => [name, [type, true, true]])),
};
const report = {
  binary: chromeOnly ? null : binary,
  wpt: { revision: wptRevision, pinnedFiles: pins, expectedChecks: 33, rust: {}, chrome: {} },
  expected,
  runs: {},
  moduleLifecycle: {},
  failures: [],
  ok: false,
};
const localScripts = {
  'boot.js': 'life.push(document.currentScript===boot ? "boot:"+document.readyState : "wrong boot currentScript");',
  'fast.js': 'document.write("<p id=must-not-exist>Bad write</p>"); ordered.push(document.currentScript===fast && fast.parentNode===null && fast.getAttribute("src")==="./must-not-fetch.js" ? "fast" : "wrong fast currentScript");',
  'second.js': 'ordered.push(document.currentScript===second ? "second" : "wrong second currentScript");',
  'integrity-mismatch.js': 'modules.push("unexpected integrity script");',
  'shared.js': 'modules.push("shared"); export const shared=19;',
  'redirected/entry.mjs': 'import { shared } from "shared"; import { leaf } from "./leaf.mjs"; modules.push("root:"+(shared+leaf)+":"+(document.currentScript===null));',
  'redirected/leaf.mjs': 'modules.push("redirect-leaf"); export const leaf=23;',
  'lazy/final.mjs': 'imports.push("tla-start"); const response = await fetch("./tla-token"); const token = await response.text(); export const answer = Number(token); export function leaf(){return import("./relative.mjs");} imports.push("tla-finish");',
  'lazy/relative.mjs': 'imports.push("nested-import"); export const value = "correct redirect base";',
};
const foreignScripts = {
  'allowed.mjs': 'import { cross } from "./leaf.mjs"; modules.push("cross:"+cross+":"+(document.currentScript===null));',
  'leaf.mjs': 'modules.push("cross-leaf"); export const cross=11;',
  'denied.mjs': 'modules.push("unexpected CORS module");',
};
const moduleLifecycle = {
  'element.mjs': 'moduleTrace.push("element-begin"); await new Promise(resolve => { window.releaseElement = resolve; }); moduleTrace.push("element-end");',
  'reason.mjs': 'await Promise.reject(originalReason);',
};
const expectedLifecycle = ['element-begin', 'element-load', 'element-end', 'window-load', 'original-reason'];
const runs = new Map(['headless', 'native', 'chrome'].map(mode => [mode, { order: [], held: {}, receipt: null }]));
const requests = [];
let pageOrigin;
let crossOrigin;

function reply(response, status, type, body, headers = {}) {
  if (response.destroyed) return;
  response.writeHead(status, { 'Content-Type': type, 'Cache-Control': 'no-store', ...headers });
  response.end(body);
}

function release(run, name) {
  const state = runs.get(run);
  const response = state.held[name];
  if (!response || !state.order.includes('release-' + name)) return;
  if (name === 'first' && !state.order.includes('second-response')) return;
  delete state.held[name];
  state.order.push(name + '-response');
  if (name === 'first') {
    reply(response, 404, js, 'ordered.push("unexpected failed script")');
  } else {
    reply(response, 200, js, localScripts['boot.js']);
  }
}

async function requestHandler(origin, request, response) {
  const path = new URL(request.url, 'http://127.0.0.1').pathname;
  requests.push({ origin, path, method: request.method, originHeader: request.headers.origin ?? null });
  if (origin === 'cross') {
    const match = /^\/foreign\/(headless|native|chrome)\/(.+)$/.exec(path);
    if (match && Object.hasOwn(foreignScripts, match[2])) {
      reply(response, 200, js, foreignScripts[match[2]],
        match[2] === 'denied.mjs' ? {} : { 'Access-Control-Allow-Origin': pageOrigin });
      return;
    }
  } else {
    if (path === '/resources/testharness.js' || path === '/resources/testharnessreport.js') {
      reply(response, 200, js, path.endsWith('testharness.js') ? harness : '');
      return;
    }
    const wpt = /^\/wpt\/(rust|chrome)\/(.+)$/.exec(path);
    if (wpt && pinned.has(wpt[2])) {
      reply(response, 200, wpt[2].endsWith('.html') ? html : js, pinned.get(wpt[2]));
      return;
    }
    const lifecycle = /^\/module-lifecycle\/(?:rust|chrome)\/(.+)$/.exec(path);
    if (lifecycle) {
      if (lifecycle[1] === 'index.html') {
        reply(response, 200, html, moduleFixture);
        return;
      }
      if (Object.hasOwn(moduleLifecycle, lifecycle[1])) {
        reply(response, 200, js, moduleLifecycle[lifecycle[1]]);
        return;
      }
    }
    const local = /^\/dynamic\/(headless|native|chrome)\/(.+)$/.exec(path);
    if (local) {
      const [, mode, name] = local;
      const state = runs.get(mode);
      if (name === 'index.html') {
        reply(response, 200, html, fixture.replaceAll('__CROSS_ROOT__', crossOrigin + '/foreign/' + mode));
        return;
      }
      if (name === 'boot.js' || name === 'first.js') {
        const key = name.slice(0, -3);
        state.order.push(key + '-request');
        state.held[key] = response;
        release(mode, key);
        return;
      }
      if (name === 'release-boot' || name === 'release-first') {
        assert.equal(request.method, 'POST');
        state.order.push(name);
        release(mode, name.slice(8));
        reply(response, 204, 'text/plain', '');
        return;
      }
      if (name === 'receipt') {
        assert.equal(request.method, 'POST');
        assert.equal(state.receipt, null);
        const chunks = [];
        let total = 0;
        for await (const chunk of request) {
          total += chunk.length;
          assert.ok(total <= 16_384);
          chunks.push(chunk);
        }
        state.receipt = JSON.parse(Buffer.concat(chunks).toString('utf8'));
        reply(response, 204, 'text/plain', '');
        return;
      }
      if (name === 'module-redirect.mjs') {
        reply(response, 302, 'text/plain', '', { Location: './redirected/entry.mjs' });
        return;
      }
      if (name === 'lazy-redirect.mjs') {
        reply(response, 302, 'text/plain', '', { Location: './lazy/final.mjs' });
        return;
      }
      if (name === 'tla-token') {
        reply(response, 200, 'text/plain', '37');
        return;
      }
      if (Object.hasOwn(localScripts, name)) {
        reply(response, 200, js, localScripts[name]);
        if (name === 'second.js') {
          state.order.push('second-response');
          release(mode, 'first');
        }
        return;
      }
    }
  }
  reply(response, 404, 'text/plain', 'Not found');
}

async function server(origin) {
  const active = createServer((request, response) => {
    requestHandler(origin, request, response).catch(error => {
      report.failures.push({ name: origin + ' server', error: String(error) });
      reply(response, 500, 'text/plain', 'Fixture server error');
    });
  });
  active.listen(0, '127.0.0.1');
  await once(active, 'listening');
  return active;
}
const pageServer = await server('page');
const crossServer = await server('cross');
pageOrigin = 'http://127.0.0.1:' + pageServer.address().port;
crossOrigin = 'http://127.0.0.1:' + crossServer.address().port;
const url = mode => pageOrigin + '/dynamic/' + mode + '/index.html';
const testUrl = (mode, file) => pageOrigin + '/wpt/' + mode + '/' + file;
const lifecycleUrl = mode => pageOrigin + '/module-lifecycle/' + mode + '/index.html';
const profile = stem => resolve(output, 'qa-' + stem + '-' + randomUUID());

async function processOutput(exe, args, stem, json = true) {
  const child = spawn(exe, args, { windowsHide: true });
  let stdout = '';
  let stderr = '';
  child.stdout.setEncoding('utf8').on('data', data => { stdout += data; });
  child.stderr.setEncoding('utf8').on('data', data => { stderr += data; });
  const timeout = setTimeout(() => child.kill(), 50_000);
  let exit;
  let spawnError;
  try {
    [exit] = await once(child, 'close');
  } catch (error) {
    spawnError = error;
  } finally {
    clearTimeout(timeout);
  }
  await Promise.all([
    writeFile(resolve(output, stem + '.stdout.txt'), stdout),
    writeFile(resolve(output, stem + '.stderr.txt'), stderr),
  ]);
  if (spawnError) throw spawnError;
  assert.equal(exit, 0, stem + ': ' + stderr.trim().slice(0, 1000));
  return json ? JSON.parse(stdout) : stdout.trim();
}

async function attempt(name, check) {
  try {
    await check();
  } catch (error) {
    report.failures.push({ name, error: String(error) });
    process.stderr.write(name + ': ' + error + '\n');
  }
}

function checkMain(mode) {
  const state = runs.get(mode);
  assert.deepEqual(state.receipt, expected, mode + ' receipt');
  const order = state.order;
  assert.ok(order.indexOf('boot-response') > order.indexOf('boot-request'));
  assert.ok(order.indexOf('boot-response') > order.indexOf('release-boot'));
  assert.ok(order.indexOf('first-response') > order.indexOf('first-request'));
  assert.ok(order.indexOf('first-response') > order.indexOf('release-first'));
  assert.ok(order.indexOf('first-response') > order.indexOf('second-response'));
  const local = '/dynamic/' + mode + '/';
  for (const name of ['index.html', ...Object.keys(localScripts), 'first.js', 'module-redirect.mjs', 'lazy-redirect.mjs', 'tla-token', 'release-boot', 'release-first', 'receipt']) {
    assert.equal(requests.filter(request => request.path === local + name).length, 1, mode + ' ' + name + ' exactly once');
  }
  for (const name of Object.keys(foreignScripts)) {
    const matches = requests.filter(request => request.path === '/foreign/' + mode + '/' + name);
    assert.equal(matches.length, 1, mode + ' ' + name + ' exactly once');
    assert.equal(matches[0].originHeader, pageOrigin, mode + ' cross-origin request');
  }
  assert.ok(!requests.some(request => ['must-not-fetch.js', 'leaf.mjs', 'relative.mjs'].some(name => request.path === local + name)));
  Object.assign(report.runs[mode], { receipt: state.receipt, order, passed: true });
}

function verifyDiagnostics(result) {
  assert.equal(result.title, 'Dynamic scripts verified');
  assert.equal(result.layout_limited, false);
  assert.equal(result.paint_limited, false);
  const messages = result.javascript.diagnostics;
  assert.equal(messages.length, 3, messages.join(' | '));
  assert.ok(messages.some(message => /404/.test(message)));
  assert.ok(messages.some(message => /integrity/i.test(message)));
  assert.ok(messages.some(message => /origin|cors/i.test(message)));
}

async function runRust() {
  report.binarySha256 = sha256(await readFile(binary));
  report.binaryVersion = await processOutput(binary, ['--version'], 'rust-version', false);
  await attempt('Rust headless dynamic scripts', async () => {
    const domPath = resolve(output, 'rust-headless.html');
    const result = await processOutput(binary, [
      '--headless', '--url', url('headless'), '--profile', profile('headless'),
      '--click-id', 'trigger', '--run-for-ms', '7000', '--dump-dom', domPath,
    ], 'rust-headless');
    report.runs.headless = { result };
    verifyDiagnostics(result);
    assert.match(await readFile(domPath, 'utf8'), /data-complete="yes"/);
    checkMain('headless');
  });
  await attempt('Rust native dynamic scripts', async () => {
    const actions = [
      { action: 'title', value: 'Dynamic script fixture ready' },
      { action: 'click', id: 'trigger' },
      { action: 'title', value: 'Dynamic scripts verified' },
    ];
    await put('native-actions.json', actions);
    const image = resolve(output, 'rust-native.png');
    const result = await processOutput(binary, [
      '--url', url('native'), '--profile', profile('native'), '--smoke-window', image,
      '--smoke-actions', resolve(output, 'native-actions.json'),
    ], 'rust-native');
    report.runs.native = { result };
    verifyDiagnostics(result);
    assert.equal(result.window_smoke, true);
    assert.ok(result.frames_presented > 0);
    const pixels = await readFile(image);
    assert.equal(pixels.subarray(0, 8).toString('hex'), '89504e470d0a1a0a');
    report.runs.native.imageSha256 = sha256(pixels);
    checkMain('native');
  });
  await attempt('Rust suspended module lifecycle', async () => {
    const domPath = resolve(output, 'rust-module-lifecycle.html');
    const result = await processOutput(binary, [
      '--headless', '--url', lifecycleUrl('rust'), '--profile', profile('module-lifecycle'),
      '--run-for-ms', '3500', '--dump-dom', domPath,
    ], 'rust-module-lifecycle');
    report.moduleLifecycle.rust = { result };
    assert.equal(result.title, 'Suspended module verified');
    assert.deepEqual(result.javascript.diagnostics, []);
    checkLifecycle('rust', await readFile(domPath, 'utf8'));
  });
  for (const [name, , count] of wptCases) {
    await attempt('Rust WPT ' + name, async () => {
      const stem = 'rust-wpt-' + name.slice(0, -5);
      const domPath = resolve(output, stem + '.html');
      const result = await processOutput(binary, [
        '--headless', '--url', testUrl('rust', name), '--profile', profile(stem),
        '--run-for-ms', '3500', '--dump-dom', domPath,
      ], stem);
      report.wpt.rust[name] = { result };
      checkWpt(name, count, await readFile(domPath, 'utf8'), report.wpt.rust[name]);
    });
  }
  report.binaryFinalSha256 = sha256(await readFile(binary));
  assert.equal(report.binaryFinalSha256, report.binarySha256, 'Browser executable changed during verification');
}

const locator = selector => ({ kind: 'css', selector });
const waitFor = (step_id, selector) => ({
  step_id, action: 'wait_for', timeout_ms: 12000, locator: locator(selector), state: 'attached',
});
async function captureChrome(url, actions, stem) {
  const scenario = {
    browser: { mode: 'launch', executable_path: chrome, headless: true },
    start_url: { url },
    actions,
    capture: { at_end: ['url', 'dom'], events: ['page-errors', 'network'] },
    command_timeout_ms: 30000,
  };
  const name = stem + '-scenario.json';
  await put(name, scenario);
  const capture = await processOutput(process.execPath, [
    rea, 'capture-browser-scenario', resolve(output, name), '--format', 'json', '--full-output',
  ], stem);
  assert.equal(capture.ok, true);
  const data = capture.data.normalized_result;
  assert.equal(data.browser.process_ownership, 'provider-owned');
  assert.equal(data.browser.cleanup, 'terminated-owned-process');
  assert.ok(data.steps.every(step => step.status === 'completed'), stem + ' Chrome checks');
  const dom = data.steps.at(-1).artifacts.dom;
  assert.equal(dom.state, 'captured');
  return { dom: dom.value.text, evidenceId: capture.data.evidence_id, browser: data.browser };
}

function checkWpt(name, count, dom, evidence) {
  const encoded = dom.match(/<pre id="wpt-receipt">([^<]*)<\/pre>/)?.[1];
  assert.ok(encoded, name + ' complete harness receipt');
  const entities = { amp: '&', lt: '<', gt: '>', quot: '"', '#39': "'" };
  evidence.subtests = JSON.parse(encoded.replace(/&(amp|lt|gt|quot|#39);/g, (_, entity) => entities[entity]));
  assert.equal(evidence.subtests.length, count, name + ' subtest count');
  assert.equal(evidence.subtests.filter(test => test.status === 'pass').length, count, JSON.stringify(evidence.subtests));
}

function checkLifecycle(mode, dom) {
  const encoded = dom.match(/<pre (?=[^>]*id="module-receipt")(?=[^>]*data-complete="yes")[^>]*>([^<]+)<\/pre>/)?.[1];
  assert.ok(encoded, mode + ' suspended module receipt');
  const observed = JSON.parse(encoded);
  assert.deepEqual(observed, expectedLifecycle, mode + ' suspended module order');
  for (const file of ['index.html', ...Object.keys(moduleLifecycle)]) {
    assert.equal(requests.filter(request => request.path === '/module-lifecycle/' + mode + '/' + file).length, 1);
  }
  report.moduleLifecycle[mode].observed = observed;
}

async function runChrome() {
  await attempt('Chrome dynamic scripts reference', async () => {
    const result = await captureChrome(url('chrome'), [
      waitFor('ready', '#trigger[data-ready]'),
      { step_id: 'user-click', action: 'click', timeout_ms: 8000, locator: locator('#trigger') },
      waitFor('receipt', '#receipt[data-complete]'),
    ], 'chrome-dynamic');
    const { dom, ...evidence } = result;
    await writeFile(resolve(output, 'chrome-dynamic.html'), dom);
    report.runs.chrome = evidence;
    assert.match(dom, /<title>Dynamic scripts verified<\/title>/);
    checkMain('chrome');
  });
  await attempt('Chrome suspended module lifecycle reference', async () => {
    const { dom, ...evidence } = await captureChrome(lifecycleUrl('chrome'), [
      waitFor('module-receipt', '#module-receipt[data-complete]'),
    ], 'chrome-module-lifecycle');
    await writeFile(resolve(output, 'chrome-module-lifecycle.html'), dom);
    report.moduleLifecycle.chrome = evidence;
    assert.match(dom, /<title>Suspended module verified<\/title>/);
    checkLifecycle('chrome', dom);
  });
  for (const [name, , count] of wptCases) {
    await attempt('Chrome WPT ' + name, async () => {
      const stem = 'chrome-wpt-' + name.slice(0, -5);
      const { dom, ...evidence } = await captureChrome(testUrl('chrome', name), [
        waitFor('wpt-receipt', '#wpt-receipt'),
      ], stem);
      await writeFile(resolve(output, stem + '.html'), dom);
      report.wpt.chrome[name] = evidence;
      checkWpt(name, count, dom, evidence);
      if (report.wpt.rust[name]?.subtests) assert.deepEqual(report.wpt.rust[name].subtests, evidence.subtests);
    });
  }
}

try {
  if (!chromeOnly) await attempt('Rust browser', runRust);
  if (withChrome) await runChrome();
} finally {
  for (const server of [pageServer, crossServer]) {
    server.closeAllConnections();
    await new Promise(resolve => server.close(resolve));
  }
  report.requests = requests;
  report.ok = report.failures.length === 0;
  await put('report.json', report);
}
console.log(JSON.stringify({
  ok: report.ok,
  file: resolve(output, 'report.json'),
  runs: Object.keys(report.runs),
  subtests: Object.fromEntries(['rust', 'chrome'].map(mode => [
    mode,
    Object.values(report.wpt[mode]).flatMap(file => file.subtests ?? []).filter(test => test.status === 'pass').length,
  ])),
  failures: report.failures,
}));
if (!report.ok) process.exitCode = 1;
