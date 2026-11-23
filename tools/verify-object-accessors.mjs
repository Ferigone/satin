import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { once } from 'node:events';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { resolve } from 'node:path';
import { promisify } from 'node:util';

const [binaryArg = 'target/release/rust-browser.exe', outputArg = 'target/object-accessor-verification',
  mode = 'both', expectedSha] = process.argv.slice(2);
assert.ok(['both', 'chrome', 'rust'].includes(mode), 'Mode must be both, chrome, or rust');
const binary = resolve(binaryArg);
const output = resolve(outputArg);
const rea = process.env.REA_CLI ??
  'C:/Users/admin/AppData/Local/npm-cache/_npx/da7caeae9db9c589/node_modules/rea-agents/scripts/rea.mjs';
const chrome = process.env.CHROME_EXE ?? 'C:/Program Files/Google/Chrome/Application/chrome.exe';
const fixture = await readFile(new URL('./fixtures/object-accessors.html', import.meta.url));
const run = promisify(execFile);
const sha = bytes => createHash('sha256').update(bytes).digest('hex');
const browserSha256 = mode === 'chrome' ? null : sha(await readFile(binary));
if (expectedSha) assert.equal(browserSha256, expectedSha.toLowerCase());
await mkdir(output, { recursive: true });

const save = (name, value) => writeFile(resolve(output, name), JSON.stringify(value, null, 2) + '\n');
const report = { ok: false, fixtureSha256: sha(fixture), browserSha256, runs: {} };
const options = { windowsHide: true, maxBuffer: 4 * 1024 * 1024 };
const title = 'Object accessors checked';
const expected = {
  descriptor: ['get total', 'set total', true, true],
  values: [4, 14, 12],
  clone: [14, true, true, false],
  date: 'accessor-date',
  stream: ['value=14', true],
  trace: ['key:get', 'key:string', 'clone:get', 'date:get valueOf', 'date:valueOf',
    'date:get toISOString', 'date:toISOString', 'stream:get write', 'stream:write'],
};

function receipt(dom) {
  assert.ok(dom.includes('<title>' + title + '</title>'), 'Finished page title');
  const field = /<pre\b(?=[^>]*\bid="receipt")(?=[^>]*\bdata-complete="yes")[^>]*>([^<]*)<\/pre>/.exec(dom)?.[1];
  assert.ok(field, 'Missing object accessor receipt');
  return JSON.parse(field);
}

const server = createServer((request, response) => {
  if (request.url !== '/accessors') return response.writeHead(404).end();
  response.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8', 'Cache-Control': 'no-store' });
  response.end(fixture);
});
server.listen(0, '127.0.0.1');
await once(server, 'listening');
const url = 'http://127.0.0.1:' + server.address().port + '/accessors';

try {
  if (mode !== 'chrome') {
    const domPath = resolve(output, 'rust-dom.html');
    const result = await run(binary, ['--headless', '--url', url, '--run-for-ms', '600',
      '--profile', resolve(output, 'profile-' + randomUUID()), '--dump-dom', domPath],
    { ...options, timeout: 20000 });
    const summary = JSON.parse(result.stdout);
    await save('rust-summary.json', summary);
    assert.equal(summary.title, title, JSON.stringify(summary.javascript));
    assert.deepEqual(summary.javascript.diagnostics, []);
    const values = receipt(await readFile(domPath, 'utf8'));
    report.runs.rust = { version: summary.version, values };
    assert.deepEqual(values, expected);
    assert.equal(sha(await readFile(binary)), browserSha256);
  }
  if (mode !== 'rust') {
    const scenario = {
      browser: { mode: 'launch', executable_path: chrome, headless: true },
      start_url: { url },
      actions: [{
        step_id: 'receipt', action: 'wait_for', state: 'attached', timeout_ms: 10000,
        locator: { kind: 'css', selector: '#receipt[data-complete="yes"]' },
      }],
      capture: { at_end: ['url', 'dom'], events: ['page-errors'] },
      command_timeout_ms: 25000,
    };
    await save('chrome-scenario.json', scenario);
    const result = await run(process.execPath, [rea, 'capture-browser-scenario',
      resolve(output, 'chrome-scenario.json'), '--format', 'json', '--full-output'],
    { ...options, timeout: 45000 });
    const capture = JSON.parse(result.stdout);
    await save('chrome-rea.json', capture);
    const normalized = capture.data.normalized_result;
    assert.ok(capture.ok);
    assert.ok(normalized.steps.every(step => step.status === 'completed'));
    assert.equal(normalized.browser.process_ownership, 'provider-owned');
    assert.equal(normalized.browser.cleanup, 'terminated-owned-process');
    const dom = normalized.steps.at(-1).artifacts.dom.value.text;
    await writeFile(resolve(output, 'chrome-dom.html'), dom);
    const values = receipt(dom);
    report.runs.chrome = { browser: normalized.browser, evidenceId: capture.data.evidence_id, values };
    assert.deepEqual(values, expected);
  }
  report.ok = true;
} catch (error) {
  report.error = String(error.stack ?? error);
  process.exitCode = 1;
} finally {
  server.closeAllConnections();
  await new Promise(done => server.close(done));
  await save('report.json', report);
}
console.log(JSON.stringify({
  ok: report.ok, browserSha256, evidence: report.runs.chrome?.evidenceId,
  report: resolve(output, 'report.json'), error: report.error,
}));
