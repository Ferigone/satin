import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { createHash } from 'node:crypto';
import { once } from 'node:events';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { resolve } from 'node:path';
import { promisify } from 'node:util';

const [executable, destination, mode] = process.argv.slice(2);
if (!executable || !destination || !['rust', 'chrome', 'both'].includes(mode)) {
  throw new Error('Usage: node tools/verify-dom-events.mjs BROWSER OUTPUT rust|chrome|both');
}

const run = promisify(execFile);
const root = resolve(destination);
const fixture = await readFile(new URL('./fixtures/dom-events.html', import.meta.url));
const digest = bytes => createHash('sha256').update(bytes).digest('hex');
const json = (name, value) =>
  writeFile(resolve(root, name), JSON.stringify(value, null, 2) + '\n');
const runOptions = { windowsHide: true, timeout: 40_000, maxBuffer: 4 * 1024 * 1024 };
const binary = resolve(executable);
const report = { fixtureSha256: digest(fixture), runs: {} };
await mkdir(root, { recursive: true });

function readChecks(html) {
  const element = html.match(/<pre\b(?=[^>]*\bid="events-report")[^>]*>([^<]*)<\/pre>/);
  assert.ok(element, 'Missing EventTarget receipt');
  const receipt = JSON.parse(element[1].replaceAll('&quot;', '"').replaceAll('&amp;', '&'));
  assert.deepEqual(receipt, { checks: Array(14).fill(true) }, JSON.stringify(receipt));
  return receipt;
}

const server = createServer((request, response) => {
  if (request.url !== '/events') {
    response.writeHead(request.url === '/favicon.ico' ? 204 : 404).end();
    return;
  }
  response.writeHead(200, { 'content-type': 'text/html; charset=utf-8' }).end(fixture);
});
server.listen(0, '127.0.0.1');
await once(server, 'listening');
const url = 'http://127.0.0.1:' + server.address().port + '/events';

try {
  if (mode !== 'chrome') {
    report.binarySha256 = digest(await readFile(binary));
    const dom = resolve(root, 'rust.html');
    const profile = resolve(root, 'native profile');
    const headless = await run(binary, [
      '--headless', '--url', url, '--click-id', 'run-events', '--dump-dom', dom,
    ], runOptions);
    report.runs.headless = { ...JSON.parse(headless.stdout), ...readChecks(await readFile(dom, 'utf8')) };
    const screenshot = resolve(root, 'native.png');
    const native = await run(binary, [
      '--url', url, '--smoke-window', screenshot, '--smoke-click-id', 'run-events', '--profile', profile,
    ], runOptions);
    report.runs.native = { ...JSON.parse(native.stdout), screenshotSha256: digest(await readFile(screenshot)) };
    for (const target of [report.runs.headless, report.runs.native]) {
      assert.equal(target.title, 'EventTarget OK');
      assert.deepEqual(target.javascript.diagnostics, []);
    }
    assert.ok(report.runs.native.frames_presented > 0);
    assert.equal(digest(await readFile(binary)), report.binarySha256);
  }
  if (mode !== 'rust') {
    const rea = process.env.REA_CLI;
    if (!rea) throw new Error('Set REA_CLI to the installed REA CLI script');
    const executable_path = process.env.CHROME_EXE ?? 'C:/Program Files/Google/Chrome/Application/chrome.exe';
    await json('chrome-scenario.json', {
      browser: { mode: 'launch', executable_path, headless: true },
      start_url: { url },
      actions: [{
        step_id: 'dispatch', action: 'click', timeout_ms: 10_000,
        locator: { kind: 'css', selector: '#run-events' },
      }],
      capture: { at_end: ['url', 'dom'], events: ['page-errors'] },
      command_timeout_ms: 25_000,
    });
    const captured = await run(process.execPath, [
      rea, 'capture-browser-scenario', resolve(root, 'chrome-scenario.json'), '--format', 'json', '--full-output',
    ], runOptions);
    const result = JSON.parse(captured.stdout);
    await json('chrome-rea.json', result);
    const normalized = result.data.normalized_result;
    const step = normalized.steps.at(-1);
    assert.equal(result.ok, true);
    assert.equal(normalized.browser.cleanup, 'terminated-owned-process');
    assert.equal(step.status, 'completed');
    assert.deepEqual(normalized.events.items, []);
    assert.equal(step.artifacts.dom.state, 'captured');
    report.runs.chrome = {
      evidenceId: result.data.evidence_id,
      browser: normalized.browser,
      ...readChecks(step.artifacts.dom.value.text),
    };
  }
  await json('report.json', report);
  console.log(JSON.stringify({ binarySha256: report.binarySha256, runs: Object.keys(report.runs) }));
} finally {
  server.closeAllConnections();
  await new Promise((done, fail) => server.close(error => error ? fail(error) : done()));
}
