// Real native window and loopback navigation; input goes through Satin's smoke handlers.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { mkdir, mkdtemp, readFile, writeFile } from 'node:fs/promises';
import http from 'node:http';
import { join, resolve } from 'node:path';
import { inflateSync } from 'node:zlib';

const [binaryInput, outputInput] = process.argv.slice(2);
assert.ok(binaryInput && outputInput, 'Usage: node tools/verify-history-ui.mjs BROWSER.exe OUTPUT');
const binary = resolve(binaryInput);
await mkdir(resolve(outputInput), { recursive: true });
const output = await mkdtemp(join(resolve(outputInput), 'history-ui-'));
const profile = join(output, 'profile');
const requests = [];
const titles = { '/a': 'History A', '/b': 'History B', '/d': 'History D', '/branch': 'History branch' };
const server = http.createServer((request, response) => {
  requests.push(`${request.method} ${request.url}`);
  const title = titles[request.url];
  if (!title) {
    response.writeHead(404).end();
    return;
  }
  const controls = request.url === '/a' ? '<a href="/b" id="next">Next</a>' : request.url === '/b' ? `
    <button id="push" type="button">Push state</button>
    <a id="next" href="/d">Next document</a>
    <a id="branch" href="/branch">New branch</a>
    <script>
      document.getElementById('push').addEventListener('click', () => {
        history.pushState({ step: 'C' }, '', '/c#fixture');
        document.title = 'History C';
      });
      window.addEventListener('popstate', event => {
        document.title = event.state && event.state.step === 'C'
          ? 'History C restored' : 'History B restored';
      });
    </script>` : '';
  response.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8', 'Cache-Control': 'no-store' });
  response.end(`<!doctype html><meta charset="utf-8"><title>${title}</title>
    <style>
      body { margin: 16px; background: white; }
      #branch { position: absolute; left: 20px; top: 72px; display: block; width: 125px; height: 30px; }
    </style>
    <h1>${title}</h1><p>Loopback fixture for Satin’s tab history.</p>${controls}`);
});

const key = (value, modifiers = {}) => ({ action: 'key', value, ...modifiers });
const click = id => ({ action: 'click', id });
const at = (x, y) => ({ action: 'click_at', x, y });
const title = value => ({ action: 'title', value });
const open = [key('f', { alt: true }), key('ArrowDown'), key('ArrowDown'), key('Enter')];

async function run(name, actions, start) {
  const screenshot = join(output, `${name}.png`);
  const actionFile = join(output, `${name}.json`);
  await writeFile(actionFile, JSON.stringify(actions, null, 2));
  const child = spawn(binary, [
    '--profile', profile, '--smoke-window', screenshot, '--smoke-actions', actionFile,
    ...(start ? ['--url', start] : []),
  ], { windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
  let stdout = '', stderr = '', expired = false;
  child.stdout.setEncoding('utf8').on('data', text => { stdout += text; });
  child.stderr.setEncoding('utf8').on('data', text => { stderr += text; });
  const timeout = setTimeout(() => { expired = true; child.kill(); }, 30000);
  try {
    const [code] = await once(child, 'close');
    assert.ok(!expired, `${name} timed out`);
    assert.equal(code, 0, stderr || stdout);
    const result = JSON.parse(stdout);
    assert.equal(result.window_smoke, true);
    assert.ok(result.frames_presented > 0);
    return { ...result, screenshot };
  } finally {
    clearTimeout(timeout);
  }
}

// Read a point in Satin's RGBA8 frame. Scale the logical test coordinates
// from the smoke window's default logical width so this also works at HiDPI.
async function pixel(path, logicalX, logicalY) {
  const png = await readFile(path);
  assert.equal(png.subarray(1, 4).toString(), 'PNG');
  const width = png.readUInt32BE(16);
  const height = png.readUInt32BE(20);
  assert.deepEqual([...png.subarray(24, 29)], [8, 6, 0, 0, 0]);
  const idat = [];
  for (let at = 8; at + 12 <= png.length;) {
    const length = png.readUInt32BE(at);
    if (png.toString('ascii', at + 4, at + 8) === 'IDAT') idat.push(png.subarray(at + 8, at + 8 + length));
    at += length + 12;
  }
  const data = inflateSync(Buffer.concat(idat));
  const x = Math.floor(logicalX * width / 1180);
  const y = Math.floor(logicalY * width / 1180);
  assert.ok(x < width && y < height);
  const stride = width * 4;
  assert.equal(data.length, (stride + 1) * height);
  let prior = Buffer.alloc(stride);
  const paeth = (a, b, c) => {
    const p = a + b - c;
    const [da, db, dc] = [Math.abs(p - a), Math.abs(p - b), Math.abs(p - c)];
    return da <= db && da <= dc ? a : db <= dc ? b : c;
  };
  for (let rowIndex = 0; rowIndex <= y; rowIndex++) {
    const start = rowIndex * (stride + 1);
    const filter = data[start];
    assert.ok(filter < 5);
    const row = data.subarray(start + 1, start + 1 + stride);
    for (let i = 0; i < stride && filter !== 0; i++) {
      const a = i >= 4 ? row[i - 4] : 0;
      const b = prior[i];
      const c = i >= 4 ? prior[i - 4] : 0;
      row[i] += filter === 1 ? a : filter === 2 ? b : filter === 3 ? (a + b) >> 1 : paeth(a, b, c);
    }
    prior = row;
  }
  return [...prior.subarray(x * 4, x * 4 + 4)];
}

server.listen(0, '127.0.0.1');
try {
  await once(server, 'listening');
  const origin = `http://127.0.0.1:${server.address().port}`;
  const mixed = await run('mixed', [
    title('History A'), click('next'), title('History B'),
    click('push'), title('History C'), click('next'), title('History D'),
    at(1143, 90), at(1100, 219), at(1100, 280), title('History D'), // Current can't be selected.
    at(1100, 222), title('History B'), // Click two steps back; this is a document load.
    ...open, key('ArrowDown'), key('Enter'), title('History C restored'), // No request for /c.
    ...open, key('Tab', { shift: true }), key('Tab'), key('Space'), title('History B restored'),
    ...open, key('Escape'), title('History B restored'),
    ...open, at(50, 210), title('History B restored'), // Dismiss outside, don't activate the link.
    click('branch'), title('History branch'),
    key('ArrowRight', { alt: true }), title('History branch'), // Forward entries were discarded.
    ...open, key('Enter'), title('History B'), ...open,
  ], `${origin}/a`);
  assert.equal(mixed.url, `${origin}/b`);
  assert.deepEqual(requests, ['GET /a', 'GET /b', 'GET /d', 'GET /b', 'GET /branch', 'GET /b']);

  const closed = await run('closed', [
    title('History B'), key('ArrowLeft', { alt: true }), title('History B'),
    ...open, key('Escape'),
  ]);
  const empty = await run('empty', [
    title('History B'), ...open, key('ArrowDown'), key('Enter'), title('History B'),
  ]);
  assert.equal(closed.url, `${origin}/b`);
  assert.equal(empty.url, `${origin}/b`);
  assert.deepEqual(requests.slice(6), ['GET /b', 'GET /b']);

  const background = [42, 41, 43, 255];
  const selected = [54, 53, 55, 255];
  const white = [255, 255, 255, 255];
  assert.deepEqual(await pixel(mixed.screenshot, 1130, 153), background);
  assert.deepEqual(await pixel(mixed.screenshot, 1130, 193), selected);
  assert.deepEqual(await pixel(mixed.screenshot, 1130, 250), background);
  assert.deepEqual(await pixel(closed.screenshot, 1130, 153), white);
  assert.deepEqual(await pixel(empty.screenshot, 1130, 193), background);
  assert.deepEqual(await pixel(empty.screenshot, 1130, 250), white);
  const report = { ok: true, output, requests, runs: { mixed, closed, empty } };
  await writeFile(join(output, 'report.json'), JSON.stringify(report, null, 2));
  console.log(JSON.stringify(report, null, 2));
} finally {
  server.close();
}
