// A single offline capability document, measured in staged Satin and Chromium.
// Advertised capabilities are not evidence that any public player used them.
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { createHash } from 'node:crypto';
import { once } from 'node:events';
import { mkdir, mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { createRequire } from 'node:module';
import { isAbsolute, join, resolve } from 'node:path';
import { promisify } from 'node:util';

const [satin, chrome, reaCli, output] = process.argv.slice(2);
assert.ok([satin, chrome, reaCli, output].every(value => value && isAbsolute(value)),
  'usage: node tools/probe-media-capabilities.mjs SATIN.exe CHROME.exe REA.mjs OUTPUT');
if (process.env.LOCALAPPDATA) {
  const installed = resolve(process.env.LOCALAPPDATA, 'RustBrowser').toLowerCase();
  assert.ok(!resolve(satin).toLowerCase().startsWith(installed + '\\'),
    'use a staged, uninstalled Satin executable');
}

const types = {
  h264: 'video/mp4; codecs="avc1.42C01E"',
  aac: 'audio/mp4; codecs="mp4a.40.2"',
  vp9: 'video/webm; codecs="vp9"',
  opus: 'audio/webm; codecs="opus"',
  av1: 'video/mp4; codecs="av01.0.05M.08"',
  mp4Opus: 'audio/mp4; codecs="opus"',
  ts: 'video/mp2t; codecs="avc1.4d001f,mp4a.40.2"',
  hls: 'application/vnd.apple.mpegurl',
  negative: 'text/html',
};
const surfaces = [
  'Worker', 'SharedWorker', 'WebAssembly', 'MediaSource', 'ManagedMediaSource',
  'MediaSourceHandle', 'SourceBuffer', 'AudioDecoder', 'VideoDecoder', 'RTCPeerConnection',
];
const html = `<!doctype html><meta charset=utf-8><title>Media capabilities fixture</title>
<video id="clip" preload="none"></video><pre id="receipt">pending</pre>
<script>
const clip = document.getElementById('clip');
const types = ${JSON.stringify(types)};
const result = { code: {}, surfaces: {}, helpers: {} };
for (const key of Object.keys(types)) {
  const type = types[key];
  result.code[key] = {
    progressive: clip.canPlayType(type),
    mse: typeof MediaSource === 'function' ? MediaSource.isTypeSupported(type) : null
  };
}
for (const key of ${JSON.stringify(surfaces)}) result.surfaces[key] = typeof globalThis[key];
result.helpers.decodingInfo = typeof navigator.mediaCapabilities === 'object'
  ? typeof navigator.mediaCapabilities.decodingInfo : 'undefined';
result.helpers.videoFrameCallback = typeof clip.requestVideoFrameCallback;
result.helpers.frameCounters = typeof clip.getVideoPlaybackQuality;
document.getElementById('receipt').textContent = JSON.stringify(result);
</script>`;

function validate(receipt) {
  assert.deepEqual(Object.keys(receipt.code), Object.keys(types));
  assert.deepEqual(Object.keys(receipt.surfaces), surfaces);
  assert.deepEqual(Object.keys(receipt.helpers), ['decodingInfo', 'videoFrameCallback', 'frameCounters']);
  for (const entry of Object.values(receipt.code)) {
    assert.ok(['', 'maybe', 'probably'].includes(entry.progressive));
    assert.ok(entry.mse === null || typeof entry.mse === 'boolean');
  }
  assert.equal(receipt.code.negative.progressive, '', 'calibration: HTML is not a media type');
  assert.notEqual(receipt.code.negative.mse, true, 'calibration: HTML is not an MSE type');
  return receipt;
}

const server = createServer((request, response) => {
  if (request.method !== 'GET' || request.url !== '/') return response.writeHead(404).end();
  response.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8', 'Cache-Control': 'no-store' }).end(html);
});
server.listen(0, '127.0.0.1');
await once(server, 'listening');
try {
  await mkdir(output, { recursive: true });
  const dir = await mkdtemp(join(output, 'capabilities-'));
  const url = `http://127.0.0.1:${server.address().port}/`;
  const capture = promisify(execFile);
  const dom = join(dir, 'satin.html');
  const { stdout } = await capture(satin, [
    '--headless', '--url', url, '--profile', join(dir, 'satin-profile'),
    '--dump-dom', dom, '--width', '1280', '--height', '800',
  ], { windowsHide: true, timeout: 25_000 });
  const satinRun = JSON.parse(stdout);
  assert.equal(satinRun.title, 'Media capabilities fixture');
  assert.equal(satinRun.javascript.diagnostics.length, 0);
  const parsed = (await readFile(dom, 'utf8')).match(/<pre id="receipt">([^<]+)<\/pre>/);
  assert.ok(parsed && parsed[1] !== 'pending', 'Satin did not execute the capability fixture');
  const satinCaps = validate(JSON.parse(parsed[1]));

  const { chromium } = createRequire(reaCli)('playwright-core');
  const browser = await chromium.launch({ executablePath: chrome, headless: true });
  let chromeCaps, chromeVersion;
  try {
    chromeVersion = browser.version();
    const page = await browser.newPage({ viewport: { width: 1280, height: 800 } });
    await page.goto(url, { waitUntil: 'load', timeout: 10_000 });
    chromeCaps = validate(JSON.parse(await page.locator('#receipt').textContent({ timeout: 5000 })));
  } finally { await browser.close(); }

  const sha256 = async path => createHash('sha256').update(await readFile(path)).digest('hex');
  const report = {
    capturedUtc: new Date().toISOString(), fixtureSha256: createHash('sha256').update(html).digest('hex'),
    types, limitation: 'advertisement only; no public site or actual playback is tested here',
    satin: { version: satinRun.version, sha256: await sha256(satin), ...satinCaps },
    chromium: { version: chromeVersion, sha256: await sha256(chrome), ...chromeCaps },
  };
  const receipt = join(dir, 'summary.json');
  await writeFile(receipt, JSON.stringify(report, null, 2) + '\n', { flag: 'wx' });
  console.log(receipt);
} finally {
  server.closeAllConnections();
  await new Promise((done, reject) => server.close(error => error ? reject(error) : done()));
}
