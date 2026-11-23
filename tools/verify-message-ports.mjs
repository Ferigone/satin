import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { once } from 'node:events';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { dirname, resolve } from 'node:path';
import { promisify } from 'node:util';

const [browserPath, outputPath, mode = 'both', expectedSha] = process.argv.slice(2);
assert.ok(browserPath && outputPath && ['both', 'rust', 'chrome'].includes(mode),
  'Usage: node tools/verify-message-ports.mjs BROWSER OUTPUT [both|rust|chrome] [BROWSER_SHA256]');
const binary = resolve(browserPath);
const output = resolve(outputPath);
const fixtures = new URL('./fixtures/message-ports/', import.meta.url);
const revision = 'fa649586f35119af9032312ef21e300e41d1c3b6';
const pinned = JSON.parse(await readFile(new URL('wpt-files.json', fixtures), 'utf8'));
const blobSupport = ['common/gc.js', 'ac43a4cfaf7735a08dfd1be177918167f815e800'];
const sha = bytes => createHash('sha256').update(bytes).digest('hex');
const gitBlob = bytes => createHash('sha1').update('blob ' + bytes.length + '\0').update(bytes).digest('hex');
const run = promisify(execFile);
const browserSha256 = mode === 'chrome' ? null : sha(await readFile(binary));
if (expectedSha) assert.equal(browserSha256, expectedSha.toLowerCase());
const save = (name, value) => writeFile(resolve(output, name), JSON.stringify(value, null, 2) + '\n');
await mkdir(output, { recursive: true });

async function original([path, gitBlobSha1]) {
  const cached = resolve(output, 'upstream', path);
  let bytes;
  try {
    bytes = await readFile(cached);
  } catch (error) {
    if (error.code !== 'ENOENT') throw error;
    const response = await fetch('https://raw.githubusercontent.com/web-platform-tests/wpt/' +
      revision + '/' + path, { signal: AbortSignal.timeout(20_000) });
    assert.ok(response.ok, 'Could not read pinned WPT file ' + path);
    bytes = Buffer.from(await response.arrayBuffer());
    assert.equal(gitBlob(bytes), gitBlobSha1, path);
    await mkdir(dirname(cached), { recursive: true });
    await writeFile(cached, bytes);
  }
  assert.equal(gitBlob(bytes), gitBlobSha1, path);
  return { path, gitBlobSha1, sha256: sha(bytes), bytes };
}

const sources = await Promise.all(pinned.map(original));
const support = await original(blobSupport);
const harness = await readFile(new URL('testharness.js', fixtures));
const authored = { path: 'tasks.js', bytes: await readFile(new URL('tasks.js', fixtures)) };
const sourcesByPath = new Map([...sources, support, authored].map(source => ['/' + source.path, source.bytes]));
sourcesByPath.set('/testharness.js', harness);
const pages = [...sources, authored].map(({ path }, id) => ({
  path,
  route: '/test/' + id,
  html: '<!doctype html><meta charset=utf-8><title>Message ports</title>' +
    '<pre id="wpt-receipt"></pre><script src="/testharness.js"></script>' +
    (path.endsWith('_Blob.any.js') ? '<script src="/common/gc.js"></script>' : '') +
    '<script src="/' + path + '"></script><script>finishMessagePortTests();</script>',
}));
const pagesByRoute = new Map(pages.map(page => [page.route, page.html]));
const server = createServer((request, response) => {
  const html = pagesByRoute.get(request.url);
  const script = sourcesByPath.get(request.url);
  if (!html && !script) {
    response.writeHead(request.url === '/favicon.ico' ? 204 : 404).end();
    return;
  }
  response.writeHead(200, {
    'Content-Type': html ? 'text/html; charset=utf-8' : 'text/javascript; charset=utf-8',
    'Cache-Control': 'no-store',
  }).end(html ?? script);
});
server.listen(0, '127.0.0.1');
await once(server, 'listening');
const origin = 'http://127.0.0.1:' + server.address().port;
const digest = ({ bytes, ...source }) => ({ ...source, bytes: bytes.length });
const report = {
  browserSha256, revision, scope: 'Window', harnessSha256: sha(harness),
  sources: sources.map(digest), support: [digest(support)],
  authored: { path: authored.path, sha256: sha(authored.bytes) },
  runs: {},
};

function readReceipt(path, dom, diagnostics = []) {
  const match = /<pre\b(?=[^>]*\bid="wpt-receipt")(?=[^>]*\bdata-complete="yes")[^>]*>([^<]*)<\/pre>/.exec(dom);
  if (!match) return { path, complete: false, results: [], diagnostics };
  const entities = { amp: '&', lt: '<', gt: '>', quot: '"', '#39': "'" };
  const receipt = JSON.parse(match[1].replace(/&(amp|lt|gt|quot|#39);/g, (_, key) => entities[key]));
  return { path, complete: receipt.results.length > 0, ...receipt, diagnostics };
}

async function nativePage(page, index) {
  const domPath = resolve(output, 'native-' + index + '.html');
  try {
    const { stdout } = await run(binary, ['--headless', '--url', origin + page.route,
      '--profile', resolve(output, 'profile-' + randomUUID()),
      '--run-for-ms', '2300', '--dump-dom', domPath],
    { windowsHide: true, timeout: 20_000, maxBuffer: 2 * 1024 * 1024 });
    const result = JSON.parse(stdout);
    return readReceipt(page.path, await readFile(domPath, 'utf8'), result.javascript.diagnostics);
  } catch (error) {
    return { path: page.path, complete: false, results: [], diagnostics: [String(error)] };
  }
}

async function reference() {
  const rea = process.env.REA_CLI;
  assert.ok(rea, 'Set REA_CLI to the installed REA CLI script');
  const chrome = process.env.CHROME_EXE ?? 'C:/Program Files/Google/Chrome/Application/chrome.exe';
  const actions = pages.flatMap((page, index) => [
    { step_id: 'load-' + index, action: 'goto', destination: { url: origin + page.route },
      wait_until: 'load', timeout_ms: 6000 },
    { step_id: 'read-' + index, action: 'wait_for', state: 'attached', timeout_ms: 5000,
      locator: { kind: 'css', selector: '#wpt-receipt[data-complete="yes"]' } },
  ]);
  const scenario = 'chrome-scenario.json';
  await save(scenario, {
    browser: { mode: 'launch', executable_path: chrome, headless: true },
    start_url: { url: origin + pages[0].route },
    actions,
    capture: { after_each_step: ['dom'], events: ['page-errors'] },
    command_timeout_ms: 60_000,
  });
  const { stdout } = await run(process.execPath, [rea, 'capture-browser-scenario',
    resolve(output, scenario), '--format', 'json', '--full-output'],
  { windowsHide: true, timeout: 70_000, maxBuffer: 8 * 1024 * 1024 });
  const capture = JSON.parse(stdout);
  await save('chrome-rea.json', capture);
  const observed = capture.data?.normalized_result;
  assert.ok(capture.ok && observed, JSON.stringify(capture).slice(0, 1500));
  assert.equal(observed.browser.process_ownership, 'provider-owned');
  assert.equal(observed.browser.cleanup, 'terminated-owned-process');
  const receipts = pages.map((page, index) => {
    const step = observed.steps.find(step => step.step_id === 'read-' + index);
    return readReceipt(page.path, step?.artifacts?.dom?.value?.text ?? '',
      step?.status === 'completed' ? [] : ['Missing completed read step']);
  });
  return { browser: observed.browser, evidenceId: capture.data.evidence_id,
    pageErrors: observed.events.items, pages: receipts };
}

function counts(receipts) {
  const result = {};
  for (const page of receipts) {
    if (!page.complete) result.incomplete = (result.incomplete ?? 0) + 1;
    for (const test of page.results) result[test.status] = (result[test.status] ?? 0) + 1;
  }
  return result;
}

try {
  if (mode !== 'rust') report.runs.chrome = await reference();
  if (mode !== 'chrome') {
    const receipts = [];
    for (let index = 0; index < pages.length; index += 3) {
      receipts.push(...await Promise.all(pages.slice(index, index + 3)
        .map((page, offset) => nativePage(page, index + offset))));
    }
    assert.equal(sha(await readFile(binary)), browserSha256);
    report.runs.rust = { pages: receipts };
  }
  for (const result of Object.values(report.runs)) {
    result.wptCounts = counts(result.pages.slice(0, sources.length));
    result.authoredCounts = counts(result.pages.slice(sources.length));
  }
  if (mode === 'both') {
    const tests = run => run.pages.map(page => [page.path, page.results.map(test => test.name)]);
    report.sameTests = JSON.stringify(tests(report.runs.rust)) === JSON.stringify(tests(report.runs.chrome));
  }
  await save('report.json', report);
  for (const [engine, result] of Object.entries(report.runs)) {
    console.log(JSON.stringify({ engine, wpt: result.wptCounts, authored: result.authoredCounts,
      evidenceId: result.evidenceId, observations: result.pages.at(-1).observations }));
    if ([result.wptCounts, result.authoredCounts].some(group => Object.keys(group).some(key => key !== 'pass'))) {
      process.exitCode = 1;
    }
  }
  if (report.sameTests === false) process.exitCode = 1;
} finally {
  server.closeAllConnections();
  await new Promise(accept => server.close(accept));
}
