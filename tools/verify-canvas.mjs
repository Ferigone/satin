import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { createServer } from 'node:http';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { inflateSync } from 'node:zlib';

const args = process.argv.slice(2);
const flags = args.filter(arg => arg.startsWith('--'));
const positional = args.filter(arg => !arg.startsWith('--'));
assert.ok(positional.length <= 2 && flags.length <= 1 &&
  flags.every(flag => ['--chrome-reference', '--chrome-reference-only'].includes(flag)),
  'Usage: node tools/verify-canvas.mjs [BROWSER.exe] [OUTPUT] [--chrome-reference | --chrome-reference-only]');
const [exe = 'target/release/rust-browser.exe', directory = 'target/verify-canvas'] = positional;
const binary = resolve(exe);
const output = resolve(directory);
const repo = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const initial = 'Płótno 2D';
const drawn = 'Płótno gotowe';
const cleared = 'Płótno wyczyszczone';
const width = 980, height = 680;
const chromeOnly = flags.includes('--chrome-reference-only');
const fragment = await readFile(resolve(repo, 'crates/engine/data/pages/canvas.html'));
const sha = bytes => createHash('sha256').update(bytes).digest('hex');
const report = { ok: false, fragmentSha256: sha(fragment), failures: [] };
await mkdir(output, { recursive: true });

function saveJson(name, value) {
  return writeFile(resolve(output, name), JSON.stringify(value, null, 2) + '\n');
}

async function command(executable, args, label) {
  const child = spawn(executable, args, { windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
  let stdout = '', stderr = '';
  child.stdout.setEncoding('utf8').on('data', chunk => { stdout += chunk; });
  child.stderr.setEncoding('utf8').on('data', chunk => { stderr += chunk; });
  const timeout = setTimeout(() => child.kill(), 60000);
  let exit;
  try {
    exit = await new Promise((done, fail) => {
      child.once('error', fail);
      child.once('close', done);
    });
  } finally {
    clearTimeout(timeout);
    await writeFile(resolve(output, label + '.stdout.txt'), stdout);
    await writeFile(resolve(output, label + '.stderr.txt'), stderr);
  }
  assert.equal(exit, 0, label + ' exited ' + exit + '\n' + stderr.trim() + '\n' + stdout.slice(-1600));
  return JSON.parse(stdout);
}

function paeth(a, b, c) {
  const p = a + b - c;
  const da = Math.abs(p - a), db = Math.abs(p - b), dc = Math.abs(p - c);
  return da <= db && da <= dc ? a : db <= dc ? b : c;
}

function png(bytes, expectScene) {
  assert.equal(bytes.subarray(0, 8).toString('hex'), '89504e470d0a1a0a');
  const width = bytes.readUInt32BE(16), height = bytes.readUInt32BE(20);
  assert.ok(width > 0 && height > 0 && width * height <= 4096 * 4096);
  const channels = bytes[25] === 6 ? 4 : bytes[25] === 2 ? 3 : 0;
  assert.ok(channels && bytes[24] === 8 && bytes[28] === 0, '8-bit noninterlaced RGB(A)');
  const idat = [];
  for (let offset = 8; offset + 12 <= bytes.length;) {
    const length = bytes.readUInt32BE(offset);
    assert.ok(offset + length + 12 <= bytes.length);
    if (bytes.toString('ascii', offset + 4, offset + 8) === 'IDAT') {
      idat.push(bytes.subarray(offset + 8, offset + 8 + length));
    }
    offset += length + 12;
  }
  const stride = width * channels;
  const scanlines = inflateSync(Buffer.concat(idat), { maxOutputLength: (stride + 1) * height });
  assert.equal(scanlines.length, (stride + 1) * height);

  // Opaque interior colors prove that the presentation path painted the bitmap.
  const palette = { sun: 0, shore: 0, bay: 0 };
  let previous = Buffer.alloc(stride);
  let row = Buffer.alloc(stride);
  for (let y = 0; y < height; y++) {
    const start = y * (stride + 1);
    const filter = scanlines[start];
    assert.ok(filter <= 4);
    for (let x = 0; x < stride; x++) {
      const a = x >= channels ? row[x - channels] : 0;
      const b = previous[x];
      const c = x >= channels ? previous[x - channels] : 0;
      let predictor = 0;
      switch (filter) {
        case 1: predictor = a; break;
        case 2: predictor = b; break;
        case 3: predictor = (a + b) >> 1; break;
        case 4: predictor = paeth(a, b, c); break;
      }
      row[x] = scanlines[start + 1 + x] + predictor;
    }
    for (let x = 0; x < stride; x += channels) {
      if (channels === 4 && row[x + 3] !== 255) continue;
      switch (row.readUIntBE(x, 3)) {
        case 0xffe9ba: palette.sun++; break;
        case 0x244f5d: palette.shore++; break;
        case 0x18836f: palette.bay++; break;
      }
    }
    [previous, row] = [row, previous];
  }
  if (expectScene) {
    assert.ok(palette.sun > 1000 && palette.shore > 1000 && palette.bay > 10000,
      'rendered canvas palette: ' + JSON.stringify(palette));
  } else {
    assert.ok(palette.sun === 0 && palette.shore === 0 && palette.bay < 1000,
      'cleared canvas palette: ' + JSON.stringify(palette));
  }
  return { width, height, bytes: bytes.length, sha256: sha(bytes), palette };
}

function checkDrawn(dom) {
  assert.ok(dom.includes('<title>' + drawn + '</title>'), 'draw title after pixel readback');
  const text = dom.match(/<p\b(?=[^>]*\bid="canvas-status")(?=[^>]*\bdata-result="drawn")[^>]*>([^<]+)<\/p>/)?.[1];
  assert.match(text ?? '', /Piksel \(28, 216\) to RGBA\(24, 131, 111, 255\)\./);
  return text;
}

function checkBrowser(result, title) {
  assert.equal(result.title, title, JSON.stringify(result.javascript));
  assert.equal(result.layout_limited, false);
  assert.equal(result.paint_limited, false);
  assert.deepEqual(result.javascript.diagnostics, []);
}

async function rust() {
  report.binary = binary;
  report.binarySha256 = sha(await readFile(binary));
  for (const [action, button, title] of [
    ['draw', 'draw-canvas', drawn],
    ['clear', 'clear-canvas', cleared],
  ]) {
    const stem = 'rust-headless-' + action;
    const dom = resolve(output, stem + '.html');
    const screenshot = resolve(output, stem + '.png');
    const result = await command(binary, [
      '--headless', '--url', 'about:canvas', '--click-id', button,
      '--profile', resolve(output, 'profile-' + randomUUID()),
      '--width', String(width), '--height', String(height),
      '--dump-dom', dom, '--screenshot', screenshot,
    ], stem);
    checkBrowser(result, title);
    const content = await readFile(dom, 'utf8');
    if (action === 'draw') {
      checkDrawn(content);
    } else {
      assert.match(content, /<p\b(?=[^>]*\bid="canvas-status")(?=[^>]*\bdata-result="cleared")[^>]*>[^<]*RGBA\(0, 0, 0, 0\)/);
    }
    const image = png(await readFile(screenshot), action === 'draw');
    assert.equal(image.width, width);
    assert.equal(image.height, height);
    report[stem] = { result, screenshot: image };
  }
  const actions = [
    { action: 'title', value: initial },
    { action: 'click', id: 'draw-canvas' },
    { action: 'title', value: drawn },
    { action: 'click', id: 'clear-canvas' },
    { action: 'title', value: cleared },
    { action: 'click', id: 'draw-canvas' },
    { action: 'title', value: drawn },
  ];
  await saveJson('native-actions.json', actions);
  const screenshot = resolve(output, 'rust-native.png');
  const result = await command(binary, [
    '--url', 'about:canvas', '--smoke-window', screenshot,
    '--smoke-actions', resolve(output, 'native-actions.json'),
    '--profile', resolve(output, 'profile-' + randomUUID()),
  ], 'rust-native');
  checkBrowser(result, drawn);
  assert.equal(result.window_smoke, true);
  assert.ok(result.frames_presented > 0);
  report.native = { result, screenshot: png(await readFile(screenshot), true), actions };
  assert.notEqual(report['rust-headless-draw'].screenshot.sha256,
    report['rust-headless-clear'].screenshot.sha256);
  assert.equal(sha(await readFile(binary)), report.binarySha256, 'binary stayed unchanged');
}

async function chrome() {
  const loader = await readFile(resolve(repo, 'crates/engine/src/loader.rs'), 'utf8');
  const style = loader.match(/const BASE_STYLE: &str = "([^"\r\n]+)";/)?.[1];
  assert.ok(style, 'built-in stylesheet');
  const source = Buffer.concat([
    Buffer.from('<!doctype html><html lang="pl"><head><meta charset="utf-8"><title>' +
      initial + '</title><style>' + style + '</style></head><body><main>'),
    fragment,
    Buffer.from('</main></body></html>'),
  ]);
  const server = createServer((request, response) => {
    if (request.url !== '/canvas') {
      response.writeHead(request.url === '/favicon.ico' ? 204 : 404).end();
      return;
    }
    response.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8', 'Cache-Control': 'no-store' });
    response.end(source);
  });
  await new Promise((done, fail) => {
    server.once('error', fail);
    server.listen(0, '127.0.0.1', done);
  });
  try {
    const selector = selector => ({ kind: 'css', selector });
    const wait = (step_id, state) => ({
      step_id, action: 'wait_for', state: 'attached', timeout_ms: 8000,
      locator: selector('#canvas-status[data-result="' + state + '"]'),
    });
    const click = (step_id, id) => ({
      step_id, action: 'click', timeout_ms: 8000, locator: selector('#' + id),
    });
    const executable = process.env.CHROME_EXE ?? 'C:/Program Files/Google/Chrome/Application/chrome.exe';
    const rea = process.env.REA_CLI ??
      'C:/Users/admin/AppData/Local/npm-cache/_npx/da7caeae9db9c589/node_modules/rea-agents/scripts/rea.mjs';
    const scenario = {
      browser: { mode: 'launch', executable_path: executable, headless: true },
      start_url: { url: 'http://127.0.0.1:' + server.address().port + '/canvas' },
      environment: { viewport: { width, height, device_scale_factor: 1 } },
      actions: [
        wait('initial-empty', 'empty'),
        click('draw', 'draw-canvas'),
        wait('drawn-rgba', 'drawn'),
        click('clear', 'clear-canvas'),
        wait('cleared-rgba', 'cleared'),
        click('redraw', 'draw-canvas'),
        wait('redrawn-rgba', 'drawn'),
      ],
      capture: { at_end: ['url', 'dom', 'screenshot'], events: ['page-errors'] },
      command_timeout_ms: 44000,
    };
    await saveJson('chrome-scenario.json', scenario);
    const run = await command(process.execPath, [
      rea, 'capture-browser-scenario', resolve(output, 'chrome-scenario.json'), '--format', 'json', '--full-output',
    ], 'chrome-rea');
    assert.equal(run.ok, true);
    const result = run.data.normalized_result;
    assert.equal(result.browser.process_ownership, 'provider-owned');
    assert.equal(result.browser.cleanup, 'terminated-owned-process');
    assert.ok(result.steps.every(step => step.status === 'completed'),
      JSON.stringify(result.steps.map(step => [step.step_id, step.status, step.error])));
    const artifacts = result.steps.at(-1).artifacts;
    assert.equal(artifacts.dom.state, 'captured');
    assert.equal(artifacts.screenshot.state, 'captured');
    const dom = artifacts.dom.value.text;
    const receipt = checkDrawn(dom);
    const screenshot = Buffer.from(artifacts.screenshot.value.data_base64, 'base64');
    const image = png(screenshot, true);
    assert.equal(image.sha256, artifacts.screenshot.value.sha256);
    assert.equal(image.width, width);
    assert.equal(image.height, height);
    await writeFile(resolve(output, 'chrome-drawn.png'), screenshot);
    await writeFile(resolve(output, 'chrome-drawn.html'), dom);
    report.chrome = {
      evidenceId: run.data.evidence_id,
      browser: result.browser,
      steps: result.steps.map(step => ({ id: step.step_id, status: step.status })),
      screenshot: image, receipt,
    };
  } finally {
    server.closeAllConnections();
    await new Promise(done => server.close(done));
  }
}

async function verify(label, operation) {
  try {
    await operation();
  } catch (error) {
    report.failures.push({ label, error: String(error.stack ?? error) });
  }
}

if (!chromeOnly) await verify('Rust Browser and native presenter', rust);
if (flags.length) await verify('owned Chrome via REA', chrome);
report.ok = report.failures.length === 0;
process.exitCode = report.ok ? 0 : 1;
await saveJson('report.json', report);
console.log(JSON.stringify({
  ok: report.ok,
  binarySha256: report.binarySha256,
  chromeEvidence: report.chrome?.evidenceId,
  report: resolve(output, 'report.json'),
  failures: report.failures,
}));
