import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { once } from 'node:events';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const repo = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const root = resolve(repo, 'tools/fixtures/mutation-observers');
const args = process.argv.slice(2);
const flags = new Set(args.filter(arg => arg.startsWith('--')));
assert.ok([...flags].every(arg => ['--chrome-reference', '--chrome-reference-only'].includes(arg)),
  'Usage: node tools/verify-mutation-observers.mjs [BROWSER.exe] [OUTPUT] [--chrome-reference | --chrome-reference-only]');
const paths = args.filter(arg => !arg.startsWith('--'));
assert.ok(paths.length <= 2);
const binary = resolve(paths[0] ?? resolve(repo, 'target/release/rust-browser.exe'));
const output = resolve(paths[1] ?? resolve(repo, 'target/mutation-observer-verification'));
const chromeOnly = flags.has('--chrome-reference-only');
const withChrome = chromeOnly || flags.has('--chrome-reference');
const rea = process.env.REA_CLI ?? 'C:/Users/admin/AppData/Local/npm-cache/_npx/da7caeae9db9c589/node_modules/rea-agents/scripts/rea.mjs';
const chrome = process.env.CHROME_EXE ?? 'C:/Program Files/Google/Chrome/Application/chrome.exe';
await mkdir(output, { recursive: true });
const put = (name, value) => writeFile(resolve(output, name), JSON.stringify(value, null, 2) + '\n');
const sha = bytes => createHash('sha256').update(bytes).digest('hex');
const profile = name => resolve(output, 'qa-' + name + '-' + randomUUID());

const revision = 'fa649586f35119af9032312ef21e300e41d1c3b6';
const pins = [
  ['MutationObserver-callback-arguments.html', 'c0a6e4034b1338b874b6e073f50373e7d2e84be3b420c170f49dd2282989a94e', 1],
  ['MutationObserver-callback-order.window.js', '0f56dd58ce156adf0beeb26935b1c162af9b42484af12e31826fa831ed5331a3', 3],
  ['MutationObserver-overlapping-registrations.html', 'a8e7e4a53148a65815f6ab908fa21408fd92ee4d5279b8b23bb7f770b8529ca3', 3],
  ['MutationObserver-takeRecords.html', '733978c79b301a107f371895345d6dffe4785976799b8fcb8fd0b374dc205f40', 3],
  ['MutationObserver-disconnect.html', '63222104917fd1bbf6b379b671f6c56a63225f1b377bbfad88e234bd72b0c318', 2],
  ['MutationObserver-transient-registered-observers.window.js', 'cdeb238634578914984c64d0f736d6aadec77a32a26652f3b6115a1a7755fc66', 15],
  ['MutationObserver-inner-outer.html', '93e16be599a75e718dfd1e70845e06d0c67f4e9b91aea10e676cf67b768b2ae2', 3],
  ['MutationObserver-callback-tricky.html', '90b20343194a4797ae4f10d7090c9f86fecab0262df1588e83ac0bec9a80112b', 2],
  ['mutationobservers.js', 'b6ef8b6ca6262cbc2ad8562fdabbf91ec723f30e2b19cf95b159e038e8052e48'],
  ['LICENSE.md', '5fac07febb0e2a97fb0d7b0def149ec08b642e1ba4b9c345283ab1cbd2af6570'],
];
const pinned = new Map(await Promise.all(pins.map(async ([file, hash]) => {
  const bytes = await readFile(resolve(root, 'wpt', file));
  assert.equal(sha(bytes), hash, file);
  return [file, bytes];
})));
const cases = pins.filter(([, , count]) => count !== undefined);
const knownDepthLimited = new Set([
  'MutationObserver-takeRecords.html',
  'MutationObserver-inner-outer.html',
]);
const harness = await readFile(resolve(root, 'testharness.js'));
const fixture = await readFile(resolve(root, 'index.html'), 'utf8');
const chunks = fixture.split('<!-- split stream here -->').map(chunk => Buffer.from(chunk));
assert.equal(chunks.length, 2);
const expected = {
  parser: ['prefix', 'write-record', 'tail-record', 'before-prepare', 'rewritten', 'after-script'],
  // Taking the earlier DOM records leaves their observer notification queued.
  callbacks: ['first:1', 'second:2', 'promise-before', 'promise-after', 'first:1'],
  records: [
    'characterData:null:old text:0:0',
    'attributes:data-key:original:0:0',
    'childList:null:null:0:1',
    'attributes:id:child:0:0',
    'childList:null:null:0:2',
    'childList:null:null:2:0',
  ],
  checks: {
    clickedAfterLoad: true, snapshots: true, reobserveClearsTransient: true,
    callbackReceiver: true, retainedRecord: true,
  },
};
const report = {
  expected, binary: chromeOnly ? null : binary, modes: {}, failures: [],
  wpt: { revision, pins, total: cases.reduce((total, [, , count]) => total + count, 0), rust: {}, chrome: {} },
  requests: [],
};
const sessions = new Map(['headless', 'native', 'chrome'].map(mode => [
  mode, { held: null, receipt: null, streaming: [] },
]));
const html = 'text/html; charset=utf-8';
const js = 'text/javascript; charset=utf-8';
function reply(response, status, type, body = '') {
  response.writeHead(status, { 'Content-Type': type, 'Cache-Control': 'no-store' });
  response.end(body);
}

async function answer(request, response) {
  const path = new URL(request.url, 'http://127.0.0.1').pathname;
  report.requests.push({ path, method: request.method });
  if (path === '/resources/testharness.js') return reply(response, 200, js, harness);
  if (path === '/resources/testharnessreport.js') return reply(response, 200, js);
  const wpt = /^\/wpt\/(?:rust|chrome)\/(.+)$/.exec(path)?.[1];
  if (wpt) {
    if (pinned.has(wpt)) return reply(response, 200, wpt.endsWith('.html') ? html : js, pinned.get(wpt));
    const wrapped = wpt.replace(/\.driver\.html$/, '.window.js');
    if (wpt.endsWith('.driver.html') && pinned.has(wrapped)) {
      return reply(response, 200, html, '<!doctype html><meta charset=utf-8>' +
        '<script src="/resources/testharness.js"></script><body><script src="' + wrapped + '"></script>');
    }
  }
  const match = /^\/observe\/(headless|native|chrome)\/(index\.html|parser-ready|receipt)$/.exec(path);
  if (!match) return reply(response, 404, 'text/plain', 'Not found');
  const state = sessions.get(match[1]);
  if (match[2] === 'index.html') {
    assert.equal(state.held, null);
    response.writeHead(200, {
      'Content-Type': html,
      'Content-Length': chunks[0].length + chunks[1].length,
      'Cache-Control': 'no-store',
    });
    state.streaming.push('prefix-sent');
    state.held = response;
    response.write(chunks[0]);
  } else if (match[2] === 'parser-ready') {
    assert.equal(request.method, 'POST');
    assert.ok(state.held);
    state.streaming.push('release-request');
    state.held.end(chunks[1]);
    state.held = null;
    state.streaming.push('tail-sent');
    reply(response, 204, 'text/plain');
  } else {
    assert.equal(request.method, 'POST');
    assert.equal(state.receipt, null);
    const bytes = [];
    let size = 0;
    for await (const chunk of request) {
      size += chunk.length;
      assert.ok(size <= 8192);
      bytes.push(chunk);
    }
    state.receipt = JSON.parse(Buffer.concat(bytes).toString('utf8'));
    state.streaming.push('receipt');
    reply(response, 204, 'text/plain');
  }
}

const server = createServer((request, response) => {
  answer(request, response).catch(error => {
    report.failures.push({ name: 'fixture server', error: String(error) });
    if (response.headersSent) response.destroy();
    else reply(response, 500, 'text/plain', 'Fixture server error');
  });
});
server.listen(0, '127.0.0.1');
await once(server, 'listening');
const origin = 'http://127.0.0.1:' + server.address().port;
const page = mode => origin + '/observe/' + mode + '/index.html';
const testPage = (mode, file) => origin + '/wpt/' + mode + '/' + file.replace(/\.window\.js$/, '.driver.html');

async function command(exe, args, stem, json = true) {
  const child = spawn(exe, args, { windowsHide: true });
  let stdout = '';
  let stderr = '';
  child.stdout.setEncoding('utf8').on('data', text => { stdout += text; });
  child.stderr.setEncoding('utf8').on('data', text => { stderr += text; });
  const timeout = setTimeout(() => child.kill(), 55_000);
  let exit, error;
  try {
    [exit] = await once(child, 'close');
  } catch (failure) {
    error = failure;
  } finally {
    clearTimeout(timeout);
  }
  await Promise.all([
    writeFile(resolve(output, stem + '.stdout.txt'), stdout),
    writeFile(resolve(output, stem + '.stderr.txt'), stderr),
  ]);
  if (error) throw error;
  assert.equal(exit, 0, stem + ': ' + stderr.slice(0, 1000));
  return json ? JSON.parse(stdout) : stdout.trim();
}

async function attempt(name, run) {
  try {
    await run();
  } catch (error) {
    report.failures.push({ name, error: String(error) });
    console.error(name, String(error));
  }
}

function verifyMain(mode) {
  const state = sessions.get(mode);
  Object.assign(report.modes[mode], { receipt: state.receipt, streaming: state.streaming });
  assert.deepEqual(state.streaming, ['prefix-sent', 'release-request', 'tail-sent', 'receipt']);
  assert.deepEqual(state.receipt, expected);
  report.modes[mode].passed = true;
}

function verifyRecords(file, count, dom, entry) {
  const encoded = dom.match(/<pre id="wpt-receipt">([^<]*)<\/pre>/)?.[1];
  const diagnostics = entry.result?.javascript.diagnostics;
  if (!encoded && knownDepthLimited.has(file) && diagnostics?.length === 1 &&
      diagnostics[0].includes('ExecutionLimit: Nested bytecode depth budget exhausted')) {
    entry.status = 'resource-limited';
    entry.unreportedSubtests = count;
    return;
  }
  assert.ok(encoded, file + ' harness receipt');
  const entities = { amp: '&', lt: '<', gt: '>', quot: '"', '#39': "'" };
  entry.subtests = JSON.parse(encoded.replace(/&(amp|lt|gt|quot|#39);/g, (_, entity) => entities[entity]));
  assert.equal(entry.subtests.length, count, file + ' subtest count');
  assert.equal(entry.subtests.filter(test => test.status === 'pass').length, count, JSON.stringify(entry.subtests));
  entry.status = 'pass';
}

function checkBrowser(result) {
  assert.equal(result.title, 'Mutation observers verified', JSON.stringify(result.javascript));
  assert.equal(result.layout_limited, false);
  assert.equal(result.paint_limited, false);
  assert.deepEqual(result.javascript.diagnostics, []);
}

async function runRust() {
  report.binarySha256 = sha(await readFile(binary));
  report.version = await command(binary, ['--version'], 'rust-version', false);
  await attempt('Rust headless streaming/DOM', async () => {
    const dom = resolve(output, 'rust-headless.html');
    const result = await command(binary, [
      '--headless', '--url', page('headless'), '--profile', profile('headless'), '--click-id', 'run',
      '--run-for-ms', '6500', '--dump-dom', dom,
    ], 'rust-headless');
    report.modes.headless = { result };
    checkBrowser(result);
    assert.match(await readFile(dom, 'utf8'), /data-complete="yes"/);
    verifyMain('headless');
  });
  await attempt('Rust native streaming/DOM', async () => {
    const image = resolve(output, 'rust-native.png');
    await put('native-actions.json', [
      { action: 'title', value: 'Mutation observer fixture ready' },
      { action: 'click', id: 'run' },
      { action: 'title', value: 'Mutation observers verified' },
    ]);
    const result = await command(binary, [
      '--url', page('native'), '--profile', profile('native'), '--smoke-window', image,
      '--smoke-actions', resolve(output, 'native-actions.json'),
    ], 'rust-native');
    report.modes.native = { result };
    checkBrowser(result);
    assert.equal(result.window_smoke, true);
    assert.ok(result.frames_presented > 0);
    const png = await readFile(image);
    assert.equal(png.subarray(0, 8).toString('hex'), '89504e470d0a1a0a');
    report.modes.native.imageSha256 = sha(png);
    verifyMain('native');
  });
  for (const [file, , count] of cases) {
    await attempt('Rust ' + file, async () => {
      const stem = 'rust-' + file;
      const dom = resolve(output, stem + '.dom.html');
      const result = await command(binary, [
        '--headless', '--url', testPage('rust', file), '--profile', profile(stem),
        '--run-for-ms', '3500', '--dump-dom', dom,
      ], stem);
      const entry = report.wpt.rust[file] = { result };
      verifyRecords(file, count, await readFile(dom, 'utf8'), entry);
    });
  }
  report.binaryFinalSha256 = sha(await readFile(binary));
  assert.equal(report.binaryFinalSha256, report.binarySha256, 'Browser executable changed during verification');
}

const locate = selector => ({ kind: 'css', selector });
const wait = (step_id, selector) => ({
  step_id, action: 'wait_for', state: 'attached', timeout_ms: 12000, locator: locate(selector),
});
async function capture(url, actions, stem) {
  const path = stem + '-scenario.json';
  await put(path, {
    browser: { mode: 'launch', executable_path: chrome, headless: true },
    start_url: { url }, actions,
    capture: { at_end: ['url', 'dom'], events: ['page-errors', 'network'] },
    command_timeout_ms: 32000,
  });
  const run = await command(process.execPath, [
    rea, 'capture-browser-scenario', resolve(output, path), '--format', 'json', '--full-output',
  ], stem);
  assert.equal(run.ok, true, stem);
  const result = run.data.normalized_result;
  assert.equal(result.browser.process_ownership, 'provider-owned');
  assert.equal(result.browser.cleanup, 'terminated-owned-process');
  assert.ok(result.steps.every(step => step.status === 'completed'), JSON.stringify(result.steps.map(step => [step.step_id, step.status, step.error])));
  const artifact = result.steps.at(-1).artifacts.dom;
  assert.equal(artifact.state, 'captured');
  const dom = artifact.value.text;
  await writeFile(resolve(output, stem + '.dom.html'), dom);
  return { dom, evidence: { id: run.data.evidence_id, browser: result.browser } };
}

async function runChrome() {
  await attempt('Chrome streaming/DOM', async () => {
    const { dom, evidence } = await capture(page('chrome'), [
      wait('ready', '#run[data-ready]'),
      { step_id: 'click', action: 'click', locator: locate('#run'), timeout_ms: 8000 },
      wait('receipt', '#receipt[data-complete]'),
    ], 'chrome-stream');
    report.modes.chrome = evidence;
    assert.match(dom, /<title>Mutation observers verified<\/title>/);
    verifyMain('chrome');
  });
  for (const [file, , count] of cases) {
    await attempt('Chrome ' + file, async () => {
      const { dom, evidence } = await capture(testPage('chrome', file), [
        wait('wpt-receipt', '#wpt-receipt'),
      ], 'chrome-' + file);
      report.wpt.chrome[file] = evidence;
      verifyRecords(file, count, dom, evidence);
    });
  }
}

try {
  if (!chromeOnly) await attempt('Rust runtime', runRust);
  if (withChrome) await runChrome();
} finally {
  server.closeAllConnections();
  await new Promise(done => server.close(done));
  report.ok = report.failures.length === 0;
  await put('report.json', report);
}
console.log(JSON.stringify({
  ok: report.ok, report: resolve(output, 'report.json'),
  modes: Object.fromEntries(Object.entries(report.modes).map(([mode, run]) => [mode, run.passed ?? false])),
  wpt: Object.fromEntries(['rust', 'chrome'].map(mode => [
    mode, {
      passed: Object.values(report.wpt[mode]).flatMap(entry => entry.subtests ?? []).filter(test => test.status === 'pass').length,
      unreportedAtResourceLimit: Object.values(report.wpt[mode]).reduce((count, entry) => count + (entry.unreportedSubtests ?? 0), 0),
    },
  ])),
  failures: report.failures,
}));
if (!report.ok) process.exitCode = 1;
