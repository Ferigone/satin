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
  'Usage: node tools/verify-named-grid.mjs [BROWSER.exe] [OUTPUT] [--chrome-reference | --chrome-reference-only]');
const [exe = 'target/release/rust-browser.exe', directory = 'target/verify-named-grid'] =
  args.filter(arg => !arg.startsWith('--'));
const binary = resolve(exe);
const output = resolve(directory);
const chromeOnly = flags.has('--chrome-reference-only');
const withChrome = chromeOnly || flags.has('--chrome-reference');
const chrome = process.env.CHROME_EXE ??
  'C:/Program Files/Google/Chrome/Application/chrome.exe';
const rea = process.env.REA_CLI ??
  'C:/Users/admin/AppData/Local/npm-cache/_npx/da7caeae9db9c589/node_modules/rea-agents/scripts/rea.mjs';
const fixture = await readFile(resolve(dirname(fileURLToPath(import.meta.url)), 'fixtures/named-grid.html'));
const viewports = [
  ['wide', 960, 680],
  ['compact', 540, 730],
];
const title = 'Grid workspace measured';
const ids = ['board','mast','queue','workspace','stock','reserve','fitted','filled',
  'fit-a','fit-b','fill-a','fill-b'];
const tolerance = 0.05;
const sha = bytes => createHash('sha256').update(bytes).digest('hex');
const report = { fixtureSha256: sha(fixture), tolerance, rust: {}, chrome: {}, comparisons: {}, failures: [] };
await mkdir(output, { recursive: true });

const server = createServer((request, response) => {
  if (!request.url.startsWith('/grid?')) {
    response.writeHead(request.url === '/favicon.ico' ? 204 : 404);
    response.end();
    return;
  }
  response.writeHead(200, {
    'Content-Type': 'text/html; charset=utf-8',
    'Cache-Control': 'no-store',
  });
  response.end(fixture);
});
await new Promise((done, fail) => {
  server.once('error', fail);
  server.listen(0, '127.0.0.1', done);
});
const page = (browser, label) => 'http://127.0.0.1:' + server.address().port + '/grid?run=' + browser + '-' + label;

function saveJson(file, value) {
  return writeFile(resolve(output, file), JSON.stringify(value, null, 2) + '\n');
}

async function command(executable, args, name, parse = true) {
  const child = spawn(executable, args, { windowsHide: true, stdio: ['ignore','pipe','pipe'] });
  let stdout = '', stderr = '';
  child.stdout.setEncoding('utf8').on('data', chunk => { stdout += chunk; });
  child.stderr.setEncoding('utf8').on('data', chunk => { stderr += chunk; });
  const timeout = setTimeout(() => child.kill(), 60000);
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
  assert.equal(code, 0, name + ' exited ' + code + '\n' + stderr.trim() + '\n' + stdout.slice(-1600));
  return parse ? JSON.parse(stdout) : stdout.trim();
}

function png(bytes, width, height) {
  assert.equal(bytes.subarray(0, 8).toString('hex'), '89504e470d0a1a0a');
  assert.equal(bytes.readUInt32BE(16), width, 'screenshot width');
  assert.equal(bytes.readUInt32BE(20), height, 'screenshot height');
  return { width, height, bytes: bytes.length, sha256: sha(bytes) };
}

function near(actual, expected, field) {
  assert.ok(Math.abs(actual - expected) <= tolerance, field + ' expected ' + expected + ', got ' + actual);
}

function measurements(dom, width) {
  const text = dom.match(/<pre\b(?=[^>]*\bid="measurements")(?=[^>]*\bdata-phase="complete")[^>]*>([^<]+)<\/pre>/)?.[1];
  assert.ok(text, 'completed geometry receipt');
  const { before, after } = JSON.parse(text);
  for (const [state, names] of [[before,ids],[after,[...ids,'added']]]) {
    assert.deepEqual(Object.keys(state).sort(), [...names].sort());
    for (const rect of Object.values(state)) {
      assert.equal(rect.length, 4);
      assert.ok(rect.every(Number.isFinite));
    }
    const compact = width <= 600;
    near(state.board[2], width - 2 * (compact ? 14 : 20), 'board width');
    assert.ok(compact
      ? state.workspace[1] < state.queue[1]
      : state.queue[0] < state.workspace[0], 'responsive named-area placement');
    const first = state['fit-a'];
    const last = state['fit-b'];
    near(first[2], last[2], 'fitted track widths');
    near(last[0] + last[2], state.fitted[0] + state.fitted[2], 'fitted shelf end');
    assert.ok(first[2] > state['fill-a'][2], 'auto-fill preserved unused tracks');
    near(state['fill-a'][2], state['fill-b'][2], 'filled track widths');
  }
  near(after.added[0], after['fit-a'][0] + after['fit-a'][2] + 10, 'inserted named line');
  near(after.added[2], after['fit-a'][2], 'inserted track width');
  assert.ok(after['fit-a'][2] < before['fit-a'][2], 'uncollapsed track resized its neighbors');
  assert.deepEqual(after['fill-b'], before['fill-b'], 'separate auto-fill shelf');
  return { before, after };
}

function compare(actual, expected) {
  let coordinates = 0, maxError = 0;
  for (const phase of ['before','after']) {
    for (const [id, rect] of Object.entries(actual[phase])) {
      rect.forEach((value, axis) => {
        const reference = expected[phase][id][axis];
        maxError = Math.max(maxError, Math.abs(value - reference));
        coordinates++;
        near(value, reference, phase + ' ' + id + ' axis ' + axis);
      });
    }
  }
  return { coordinates, maxError };
}

async function attempt(label, callback) {
  try {
    await callback();
  } catch (error) {
    report.failures.push(label + ': ' + error.stack);
  }
}

async function rust(label, width, height) {
  const stem = 'rust-' + label;
  const dom = resolve(output, stem + '.html');
  const screenshot = resolve(output, stem + '.png');
  const result = await command(binary, [
    '--headless', '--url', page('rust', label), '--width', String(width), '--height', String(height),
    '--profile', resolve(output, stem + '-profile'), '--click-id', 'add', '--dump-dom', dom,
    '--screenshot', screenshot,
  ], stem);
  assert.equal(result.title, title, JSON.stringify(result.javascript));
  assert.equal(result.layout_limited, false);
  assert.equal(result.paint_limited, false);
  assert.deepEqual(result.javascript.diagnostics, []);
  report.rust[label] = {
    result, screenshot: png(await readFile(screenshot), width, height),
    geometry: measurements(await readFile(dom, 'utf8'), width),
  };
}

async function reference(label, width, height) {
  const stem = 'chrome-' + label;
  const scenario = stem + '-scenario.json';
  const wait = (step, selector) => ({
    step_id:step, action:'wait_for', state:'attached', timeout_ms:12000,
    locator:{kind:'css',selector},
  });
  await saveJson(scenario, {
    browser: { mode:'launch', executable_path:chrome, headless:true },
    start_url: { url:page('chrome', label) },
    environment: { viewport:{width,height,device_scale_factor:1} },
    actions: [
      wait('ready', '#add[data-phase="ready"]'),
      { step_id:'add', action:'click', timeout_ms:8000, locator:{kind:'css',selector:'#add'} },
      wait('measured', '#measurements[data-phase="complete"]'),
    ],
    capture: { at_end:['url','dom','screenshot'], events:['page-errors'] },
    command_timeout_ms: 44000,
  });
  const run = await command(process.execPath, [
    rea, 'capture-browser-scenario', resolve(output, scenario), '--format', 'json', '--full-output',
  ], stem);
  const result = run.data.normalized_result;
  assert.equal(result.browser.process_ownership, 'provider-owned');
  assert.equal(result.browser.cleanup, 'terminated-owned-process');
  assert.ok(result.steps.every(step => step.status === 'completed'),
    JSON.stringify(result.steps.map(step => [step.step_id, step.status, step.error])));
  const artifacts = result.steps.at(-1).artifacts;
  assert.equal(artifacts.dom.state, 'captured');
  assert.equal(artifacts.screenshot.state, 'captured');
  const dom = artifacts.dom.value.text;
  assert.match(dom, /<title>Grid workspace measured<\/title>/);
  const screenshot = Buffer.from(artifacts.screenshot.value.data_base64, 'base64');
  assert.equal(sha(screenshot), artifacts.screenshot.value.sha256);
  await writeFile(resolve(output, stem + '.html'), dom);
  await writeFile(resolve(output, stem + '.png'), screenshot);
  report.chrome[label] = {
    evidence: run.data.evidence_id, browser: result.browser,
    screenshot: png(screenshot, width, height), geometry: measurements(dom, width),
  };
}

try {
  if (!chromeOnly) {
    report.binarySha256 = sha(await readFile(binary));
    report.version = await command(binary, ['--version'], 'version', false);
    for (const [label, width, height] of viewports) {
      await attempt('Rust ' + label, () => rust(label, width, height));
    }
    report.binaryFinalSha256 = sha(await readFile(binary));
    assert.equal(report.binaryFinalSha256, report.binarySha256, 'executable changed during verification');
  }
  if (withChrome) {
    for (const [label, width, height] of viewports) {
      await attempt('Chrome ' + label, () => reference(label, width, height));
      if (report.rust[label] && report.chrome[label]) {
        await attempt('Geometry ' + label, () => {
          report.comparisons[label] = compare(report.rust[label].geometry, report.chrome[label].geometry);
        });
      }
    }
  }
} finally {
  server.closeAllConnections();
  await new Promise(done => server.close(done));
  report.ok = report.failures.length === 0;
  await saveJson('report.json', report);
}

console.log(JSON.stringify({ ok:report.ok, report:resolve(output, 'report.json'),
  comparisons:report.comparisons, failures:report.failures }));
if (!report.ok) process.exitCode = 1;
