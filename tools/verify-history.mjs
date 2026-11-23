// Exercise the same page and real navigations in Rust and an owned Chrome.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { once } from 'node:events';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import http from 'node:http';
import { resolve } from 'node:path';

const [binaryInput, outputInput, flag, reaInput] = process.argv.slice(2);
assert.ok(binaryInput && outputInput && (!flag || flag === '--rea' && reaInput),
  'Usage: node tools/verify-history.mjs BROWSER.exe OUTPUT [--rea PATH]');
const binary = resolve(binaryInput);
const output = resolve(outputInput);
const rea = reaInput && resolve(reaInput);
const chrome = resolve(process.env.ProgramFiles ?? 'C:/Program Files', 'Google/Chrome/Application/chrome.exe');
await mkdir(output, { recursive: true });
const fixture = await readFile(new URL('./fixtures/history.html', import.meta.url));
const digest = bytes => createHash('sha256').update(bytes).digest('hex');
const expected = {
  trace: ['push', 'pop:-:#three', 'sync:#three', 'microtask', 'hash:#two>#three',
    'pop:nested:#two', 'hash:#three>#two', 'pop:-:#three', 'hash:#two>#three'],
  length: 2, path: 'finish', search: '?phase=done', state: 'final', document: true,
};
const report = { ok: false, binarySha256: digest(await readFile(binary)), fixtureSha256: digest(fixture),
  requests: [], runs: {}, errors: [] };
const receipts = new Map();
const destinations = new Map();

function replaceSource() {
  return `<!doctype html><meta charset="utf-8"><title>Replacement pending</title><script>
    window.addEventListener('load', () => location.replace('destination?initial=' + history.length));
  </script>`;
}

function destinationSource(reload) {
  return `<!doctype html><meta charset="utf-8"><title>Reload pending</title>
    <h1>Location replacement</h1><p id="result">Waiting</p>
    <p>Replacing and reloading the current history entry.</p><script>
    if (Number(new URL(location.href).searchParams.get('initial')) !== history.length) {
      document.title='Wrong history length';
    } else if (${reload}) {
      window.addEventListener('load', () => history.go(0));
    } else {
      document.title='Location replacement OK';
      document.getElementById('result').setAttribute('data-done','yes');
      document.getElementById('result').textContent='Location replacement OK';
    }
  </script>`;
}

const server = http.createServer(async (request, response) => {
  const url = new URL(request.url, 'http://127.0.0.1');
  const [, mode, ...rest] = url.pathname.split('/');
  const path = rest.join('/');
  report.requests.push({ mode, path, search: url.search, method: request.method });
  const reply = (data, type = 'text/plain', status = 200) => {
    response.writeHead(status, {'Content-Type': type + '; charset=utf-8', 'Cache-Control': 'no-store'});
    response.end(data);
  };
  if (path === 'start') return reply(fixture, 'text/html');
  if (path === 'nested/relative') return reply('rewritten base');
  if (path === 'nested/assets/relative') return reply('frozen base');
  if (path === 'receipt' && request.method === 'POST') {
    try {
      const chunks = [];
      for await (const chunk of request) chunks.push(chunk);
      receipts.set(mode, JSON.parse(Buffer.concat(chunks).toString('utf8')));
      return reply('ok');
    } catch (error) {
      return reply(String(error), 'text/plain', 400);
    }
  }
  if (path === 'replace') return reply(replaceSource(), 'text/html');
  if (path === 'destination') {
    const count = (destinations.get(mode) ?? 0) + 1;
    destinations.set(mode, count);
    return reply(destinationSource(count === 1), 'text/html');
  }
  return reply('Not found', 'text/plain', 404);
});

async function processCapture(executable, args, stem) {
  const child = spawn(executable, args, { windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
  let stdout = '', stderr = '', expired = false;
  child.stdout.setEncoding('utf8').on('data', text => { stdout += text; });
  child.stderr.setEncoding('utf8').on('data', text => { stderr += text; });
  const timeout = setTimeout(() => { expired = true; child.kill(); }, 35000);
  try {
    const [code] = await once(child, 'close');
    await writeFile(resolve(output, stem + '.out'), stdout);
    assert.ok(!expired, stem + ' timed out');
    assert.equal(code, 0, stderr || stdout);
    return JSON.parse(stdout);
  } finally {
    clearTimeout(timeout);
  }
}

function assertRequests(mode) {
  assert.deepEqual(receipts.get(mode), expected, mode + ' exact receipt');
  assert.deepEqual(report.requests.filter(entry => entry.mode === mode).map(entry => entry.method + ' ' + entry.path),
    ['GET start', 'GET nested/relative', 'GET nested/assets/relative', 'POST receipt']);
}

function assertReplacement(mode) {
  const requests = report.requests.filter(entry => entry.mode === mode);
  assert.deepEqual(requests.map(entry => entry.method + ' ' + entry.path), ['GET replace', 'GET destination', 'GET destination']);
  assert.match(requests[1].search, /^\?initial=[0-9]+$/);
  assert.equal(requests[2].search, requests[1].search);
}

async function captureChrome(url, actions, stem) {
  const path = resolve(output, stem + '.json');
  await writeFile(path, JSON.stringify({
    browser: {mode:'launch', executable_path:chrome, headless:true}, start_url: {url}, actions,
    capture: {at_end:['url','dom'], events:['page-errors','network']}, command_timeout_ms:28000,
  }));
  const capture = await processCapture(process.execPath,
    [rea, 'capture-browser-scenario', path, '--format', 'json', '--full-output'], stem);
  assert.equal(capture.ok, true);
  const data = capture.data.normalized_result;
  assert.ok(data.steps.every(step => step.status === 'completed'), stem + ' Chrome steps');
  assert.equal(data.browser.cleanup, 'terminated-owned-process');
  return {evidenceId:capture.data.evidence_id, browser:data.browser, dom:data.steps.at(-1).artifacts.dom.value.text};
}

async function attempt(name, test) {
  try {
    await test();
  } catch (error) {
    report.errors.push({name, error:String(error)});
  }
}

try {
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  const url = (mode, path) => `http://127.0.0.1:${server.address().port}/${mode}/${path}`;
  const wait = (selector, id) => ({step_id:id, action:'wait_for', state:'attached', timeout_ms:12000,
    locator:{kind:'css', selector}});

  for (const mode of ['headless','native']) {
    await attempt(mode, async () => {
      const args = mode === 'headless'
        ? ['--headless','--click-id','start','--run-for-ms','2000']
        : ['--smoke-window', resolve(output,'rust-native.png'),'--smoke-click-id','start'];
      const result = await processCapture(binary, ['--url', url(mode,'start'), '--profile', resolve(output, mode+' profile'), ...args], mode);
      report.runs[mode] = result;
      assert.equal(result.title, 'History pipeline OK');
      assert.equal(new URL(result.url).pathname, `/${mode}/finish`);
      assert.deepEqual(result.javascript.diagnostics, []);
      if (mode === 'native') assert.ok(result.frames_presented >= 1);
      assertRequests(mode);
    });
    await attempt(mode + '-replace', async () => {
      const run = mode + '-replace';
      const args = mode === 'headless' ? ['--headless'] : ['--smoke-window', resolve(output,'replace-native.png')];
      const result = await processCapture(binary, ['--url', url(run,'replace'), '--profile', resolve(output, run+' profile'), ...args], run);
      report.runs[run] = result;
      assert.equal(result.title, 'Location replacement OK');
      assert.equal(new URL(result.url).pathname, `/${run}/destination`);
      assert.deepEqual(result.javascript.diagnostics, []);
      assertReplacement(run);
    });
  }

  if (rea) {
    await attempt('chrome', async () => {
      const result = await captureChrome(url('chrome','start'), [
        {step_id:'click', action:'click', timeout_ms:8000, locator:{kind:'css', selector:'#start'}},
        wait('#receipt[data-done]', 'receipt'),
      ], 'chrome');
      report.runs.chrome = {evidenceId:result.evidenceId, browser:result.browser};
      assert.match(result.dom, /<title>History pipeline OK<\/title>/);
      assertRequests('chrome');
    });
    await attempt('chrome-replace', async () => {
      const run = 'chrome-replace';
      const result = await captureChrome(url(run,'replace'), [wait('#result[data-done]','replace')], run);
      report.runs[run] = {evidenceId:result.evidenceId, browser:result.browser};
      assert.match(result.dom, /<title>Location replacement OK<\/title>/);
      assertReplacement(run);
    });
  }
  assert.equal(digest(await readFile(binary)), report.binarySha256, 'executable changed during verification');
} finally {
  await new Promise(done => { server.close(done); server.closeAllConnections(); });
}
report.receipts = Object.fromEntries(receipts);
report.ok = report.errors.length === 0;
await writeFile(resolve(output,'report.json'), JSON.stringify(report, null, 2) + '\n');
console.log(JSON.stringify({ ok:report.ok, runs:Object.keys(report.runs), errors:report.errors, sha256:report.binarySha256 }));
if (!report.ok) process.exitCode = 1;
