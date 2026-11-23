import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { once } from 'node:events';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { basename, resolve } from 'node:path';
import { promisify } from 'node:util';

const [binaryArg, outputArg, mode = 'both', expectedSha] = process.argv.slice(2);
if (!binaryArg || !outputArg || !['both', 'chrome', 'rust'].includes(mode)) {
  throw new Error('Usage: node tools/verify-structured-clone-wpt.mjs BROWSER OUTPUT both|chrome|rust [RELEASE_SHA256]');
}
const binary = resolve(binaryArg);
const output = resolve(outputArg);
const revision = 'fa649586f35119af9032312ef21e300e41d1c3b6';
const directory = 'html/webappapis/structured-clone/';
const pinned = [
  ['common/sab.js', 'a3ea610e165d0d56ef1faab2ed49186d4e03c392'],
  [directory + 'structured-clone-battery-of-tests.js', '9a1a710d8a57d4e3574cd92a9d905b2ea69242b9'],
  [directory + 'structured-clone-battery-of-tests-with-transferables.js', 'c9ffa1e54de9bf6394f02ff4916476f0ed74512b'],
  [directory + 'structured-clone-battery-of-tests-harness.js', '00a86fa74b9a968dabae400f57dd32ffb836aa2c'],
  [directory + 'structured-clone.any.js', '1358a71fc03d4e06fe8ae2701fc647d4892e9e5d'],
];
const sha256 = bytes => createHash('sha256').update(bytes).digest('hex');
const gitHash = bytes => createHash('sha1').update('blob ' + bytes.length + '\0').update(bytes).digest('hex');
const save = (name, value) => writeFile(resolve(output, name), JSON.stringify(value, null, 2) + '\n');
const run = promisify(execFile);
const options = { windowsHide: true, timeout: 45_000, maxBuffer: 6 * 1024 * 1024 };
const browserSha256 = mode === 'chrome' ? null : sha256(await readFile(binary));
if (expectedSha) assert.equal(browserSha256, expectedSha.toLowerCase());
const cache = resolve(output, 'upstream');
await mkdir(cache, { recursive: true });

async function loadBytes(path, expected) {
  const file = resolve(cache, basename(path));
  let bytes;
  try {
    bytes = await readFile(file);
  } catch (error) {
    if (error.code !== 'ENOENT') throw error;
    const response = await fetch('https://raw.githubusercontent.com/web-platform-tests/wpt/' +
      revision + '/' + path, { signal: AbortSignal.timeout(15_000) });
    assert.ok(response.ok, 'Could not read pinned WPT file ' + path + ' (HTTP ' + response.status + ')');
    bytes = Buffer.from(await response.arrayBuffer());
    assert.equal(gitHash(bytes), expected, path);
    await writeFile(file, bytes);
  }
  assert.equal(gitHash(bytes), expected, path);
  return bytes;
}

const sources = await Promise.all(pinned.map(async ([path, gitBlobSha1]) => {
  const bytes = await loadBytes(path, gitBlobSha1);
  return { path, gitBlobSha1, sha256: sha256(bytes), bytes };
}));
const harness = await readFile(new URL('./fixtures/web-streams/testharness.js', import.meta.url));
const html = '<!doctype html><meta charset=utf-8><title>Structured clone WPT</title>' +
  '<pre id="wpt-receipt"></pre><script src="/testharness.js"></script>' +
  sources.map(source => '<script src="/' + source.path + '"></script>').join('') +
  '<script>runStreamsTests();</script>';
const routes = new Map([
  ['/', { body: html, mime: 'text/html; charset=utf-8' }],
  ['/testharness.js', { body: harness, mime: 'text/javascript' }],
  ...sources.map(source => ['/' + source.path, { body: source.bytes, mime: 'text/javascript' }]),
]);
const report = {
  browserSha256, revision, harnessSha256: sha256(harness),
  sources: sources.map(({ bytes, ...source }) => ({ ...source, bytes: bytes.length })),
  runs: {},
};
const server = createServer((request, response) => {
  const resource = routes.get(request.url);
  if (!resource) {
    response.writeHead(request.url === '/favicon.ico' ? 204 : 404).end();
    return;
  }
  response.writeHead(200, { 'Content-Type': resource.mime, 'Cache-Control': 'no-store',
    'Cross-Origin-Opener-Policy': 'same-origin', 'Cross-Origin-Embedder-Policy': 'require-corp' });
  response.end(resource.body);
});
server.listen(0, '127.0.0.1');
await once(server, 'listening');
const url = 'http://127.0.0.1:' + server.address().port + '/';

function readCases(dom) {
  const match = /<pre\b(?=[^>]*\bid="wpt-receipt")(?=[^>]*\bdata-complete="yes")[^>]*>([^<]*)<\/pre>/.exec(dom);
  assert.ok(match, 'Missing final WPT receipt');
  const entities = { amp: '&', lt: '<', gt: '>', quot: '"', '#39': "'" };
  const cases = JSON.parse(match[1].replace(/&(amp|lt|gt|quot|#39);/g, (_, key) => entities[key]));
  const counts = {};
  for (const item of cases) {
    assert.ok(['pass', 'fail', 'precondition', 'timeout'].includes(item.status));
    counts[item.status] = (counts[item.status] ?? 0) + 1;
  }
  return { cases, counts };
}

try {
  if (mode !== 'chrome') {
    const domPath = resolve(output, 'rust-dom.html');
    const { stdout } = await run(binary, ['--headless', '--url', url,
      '--profile', resolve(output, 'profile-' + randomUUID()), '--run-for-ms', '12000',
      '--dump-dom', domPath], options);
    const result = JSON.parse(stdout);
    await save('rust-process.json', result);
    const dom = await readFile(domPath, 'utf8');
    report.runs.rust = { diagnostics: result.javascript.diagnostics, ...readCases(dom) };
    assert.equal(sha256(await readFile(binary)), browserSha256);
  }
  if (mode !== 'rust') {
    const rea = process.env.REA_CLI;
    if (!rea) throw new Error('Set REA_CLI to the installed REA CLI script');
    const chrome = process.env.CHROME_EXE ?? 'C:/Program Files/Google/Chrome/Application/chrome.exe';
    await save('chrome-scenario.json', {
      browser: { mode: 'launch', executable_path: chrome, headless: true },
      start_url: { url },
      actions: [{
        step_id: 'battery', action: 'wait_for', state: 'attached', timeout_ms: 25_000,
        locator: { kind: 'css', selector: '#wpt-receipt[data-complete="yes"]' },
      }],
      capture: { at_end: ['url', 'dom'], events: ['page-errors'] },
      command_timeout_ms: 36_000,
    });
    const { stdout } = await run(process.execPath, [rea, 'capture-browser-scenario',
      resolve(output, 'chrome-scenario.json'), '--format', 'json', '--full-output'], options);
    const result = JSON.parse(stdout);
    await save('chrome-rea.json', result);
    const observed = result.data.normalized_result;
    const final = observed.steps.at(-1);
    assert.ok(result.ok);
    assert.equal(observed.browser.process_ownership, 'provider-owned');
    assert.equal(observed.browser.cleanup, 'terminated-owned-process');
    assert.equal(final.status, 'completed');
    report.runs.chrome = { browser: observed.browser, evidenceId: result.data.evidence_id,
      diagnostics: observed.events.items, ...readCases(final.artifacts.dom.value.text) };
  }
  await save('report.json', report);
  for (const result of Object.values(report.runs)) {
    assert.equal(result.cases.length, 152, JSON.stringify(result.diagnostics));
  }
  if (mode === 'both') {
    const names = result => result.cases.map(item => item.name);
    assert.deepEqual(names(report.runs.rust), names(report.runs.chrome));
  }
  console.log(JSON.stringify(Object.fromEntries(Object.entries(report.runs).map(([key, result]) =>
    [key, { registered: result.cases.length, counts: result.counts, diagnostics: result.diagnostics }]))));
} finally {
  server.closeAllConnections();
  await new Promise(accept => server.close(accept));
}
