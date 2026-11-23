// Node only serves owned fixtures and orchestrates child processes. Page JS runs
// in the independently implemented Rust browser, or isolated headless Chrome
// when --chrome-reference is requested. No OS input or existing tabs are used.
import http from 'node:http';
import { spawn } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { access, mkdir, readFile, readdir, writeFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import assert from 'node:assert/strict';

const repo = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const options = new Map();
const positional = [];
for (let i = 2; i < process.argv.length; i++) {
  const argument = process.argv[i];
  if (argument === '--chrome-reference' || argument === '--chrome-reference-only') options.set(argument, true);
  else if (['--profile', '--chrome', '--rea', '--timeout-ms'].includes(argument)) {
    assert.ok(process.argv[i + 1] && !process.argv[i + 1].startsWith('--'), `Missing value for ${argument}`);
    options.set(argument, process.argv[++i]);
  } else if (argument.startsWith('--')) throw new Error(`Unknown option ${argument}`);
  else positional.push(argument);
}
assert.ok(positional.length <= 2, 'Usage: node tools/verify-forms.mjs [BROWSER.exe] [EVIDENCE_DIR] [--chrome-reference | --chrome-reference-only] [--profile FRESH_QA_DIR] [--chrome PATH] [--rea PATH] [--timeout-ms N]');
const chromeOnly = options.has('--chrome-reference-only');
if (chromeOnly) options.set('--chrome-reference', true);
const runId = `forms-${Date.now()}-${randomUUID().slice(0, 8)}`;
const binary = resolve(positional[0] ?? resolve(repo, 'target/debug/rust-browser.exe'));
const output = resolve(positional[1] ?? resolve(repo, '../../work/alpha4-forms', runId));
const profile = resolve(options.get('--profile') ?? resolve(output, 'qa-profiles', runId));
const timeoutMs = Number(options.get('--timeout-ms') ?? 90000);
assert.ok(Number.isSafeInteger(timeoutMs) && timeoutMs >= 1000 && timeoutMs <= 300000, 'timeout-ms must be 1000..300000');
await access(binary);
await mkdir(output, { recursive: true });
try {
  assert.deepEqual(await readdir(profile), [], 'The first launch requires a fresh, empty QA profile; no existing profile will be deleted or reused');
} catch (error) {
  if (error.code !== 'ENOENT') throw error;
}
await mkdir(profile, { recursive: true });
await writeFile(resolve(profile, 'owned-forms-fixture.txt'), `${runId}\nSynthetic local HTTP data only.\n`);

const values = {
  username: 'Jan Kowalski żółw',
  password: 'synthetic-password-42',
  note: 'Line one\nLinia żółw & +',
};
const expectedEntries = [
  ['username', values.username], ['password', values.password], ['subscribe', 'yes'],
  ['choice', 'b'], ['plan', 'beta'], ['note', values.note.replace(/\n/g, '\r\n')], ['send', 'go'],
];
const expectedBody = new URLSearchParams(expectedEntries).toString();
const requests = [];
const submissions = new Map();
const checks = [];
const sources = new Map();
let origin = '';
let phase = 'rust-initial';
const cookies = {
  visible: 'owned-public', private: 'owned-http-only', ephemeral: 'owned-session',
  script: 'owned-js', after_post: 'owned-post', from_fetch: 'owned-fetch',
};
const cookieMap = header => Object.fromEntries((header ?? '').split(';').filter(Boolean).map(pair => {
  const at = pair.indexOf('=');
  return [pair.slice(0, at).trim(), pair.slice(at + 1).trim()];
}));
const recordCheck = (name, condition, detail = '') => {
  checks.push({ name, pass: Boolean(condition), detail });
  return Boolean(condition);
};
const html = (title, body, script) => `<!doctype html><meta charset="utf-8"><title>${title}</title>
<style>body{margin:24px;font-family:Segoe UI;color:#16342c;background:#f7faf9}main{max-width:780px}label{display:block;margin:12px 0}input,select,textarea,button{font-size:16px}input[type=text],input[type=password],textarea,select{width:320px}textarea{height:70px}button{padding:10px 18px;background:#15765f;color:white;border:1px solid #0b5442}pre{white-space:pre-wrap}#status{padding:14px;background:#e0f1e9}</style>
<main><h1>Independent forms and cookies</h1>${body}</main><script>${script}</script>`;
const helpers = `function check(value,message){if(!value)throw new Error(message);}function hasCookie(name,value){return ('; '+document.cookie+';').indexOf('; '+name+'='+value+';')>=0;}function fail(error){document.title='Forms FAIL';document.getElementById('status').textContent=String(error);}`;
function formSource(engine) {
  return html('Forms pending', `
<form id="form" action="/submit?engine=${engine}" method="post" enctype="application/x-www-form-urlencoded">
<label>Text input <input id="username" name="username" type="text" value="replace-me"></label>
<label>Password <input id="password" name="password" type="password"></label>
<label><input id="subscribe" name="subscribe" type="checkbox" value="yes"> Subscribe</label>
<label><input id="radio_a" name="choice" type="radio" value="a" checked> Choice A</label>
<label><input id="radio_b" name="choice" type="radio" value="b"> Choice B</label>
<label>Plan <select id="plan" name="plan"><option value="alpha">Alpha</option><option value="beta">Beta</option><option value="gamma">Gamma</option></select></label>
<label>Note <textarea id="note" name="note">replace note</textarea></label>
<button id="submit" name="send" value="go">Submit owned form</button></form>
<p id="status">Waiting for browser handlers.</p>`, `${helpers}
try{
check(hasCookie('visible',${JSON.stringify(cookies.visible)}),'visible response cookie');
check(hasCookie('ephemeral',${JSON.stringify(cookies.ephemeral)}),'session cookie');
check(document.cookie.indexOf('private=')<0,'HttpOnly was exposed');
document.cookie='script=${cookies.script}; Path=/; Max-Age=3600; SameSite=Lax';
check(hasCookie('script',${JSON.stringify(cookies.script)}),'document.cookie setter');
document.getElementById('status').textContent='Ready: text, password, checkbox, radio, select and textarea.';
document.title='Forms ready';
}catch(error){fail(error);}`);
}
function receiptSource(engine) {
  return html('Forms receipt pending', '<h2>POST redirected with 303</h2><p id="status">Waiting for Promise.all, Fetch and JSON.</p><pre id="result"></pre>', `${helpers}
try{
check(document.cookie.indexOf('private=')<0&&document.cookie.indexOf('after_post=')<0,'navigation HttpOnly secrecy');
Promise.all([
 fetch('/api/receipt?engine=${engine}').then(function(response){check(response.headers.get('set-cookie')===null,'Fetch Set-Cookie exposed');return response.json();}),
 fetch('/api/extra?engine=${engine}').then(function(response){return response.json();}),
 Promise.resolve('native promise')
]).then(function(parts){
check(parts.length===3&&parts[0].ok&&parts[1].marker==='second response'&&parts[2]==='native promise','Promise.all ordering and JSON');
check(JSON.parse(JSON.stringify(parts[0])).ok,'JSON round trip');
check(document.cookie.indexOf('private=')<0&&document.cookie.indexOf('after_post=')<0&&document.cookie.indexOf('from_fetch=')<0,'HttpOnly after Fetch');
check(hasCookie('visible','${cookies.visible}')&&hasCookie('script','${cookies.script}'),'public cookies after navigation');
document.getElementById('result').textContent=JSON.stringify({ok:true,form:parts[0],promise:parts[2],httpOnlyHidden:true});
document.getElementById('result').setAttribute('data-ready','yes');
document.getElementById('status').textContent='Text and password entered; check/radio/select changed; POST, 303, cookies, Promise.all and JSON verified.';
document.title='Forms pipeline OK';
}).catch(fail);
}catch(error){fail(error);}`);
}
function reopenSource(engine) {
  return html('Forms reopen pending', '<h2>Fresh process, same QA profile</h2><p id="status">Checking persisted cookies.</p><pre id="result"></pre>', `${helpers}
try{
check(hasCookie('visible','${cookies.visible}')&&hasCookie('script','${cookies.script}'),'public persisted cookies');
check(document.cookie.indexOf('private=')<0&&document.cookie.indexOf('after_post=')<0&&document.cookie.indexOf('from_fetch=')<0,'persisted HttpOnly secrecy');
check(document.cookie.indexOf('ephemeral=')<0,'session cookie survived process exit');
Promise.all([fetch('/api/persist?engine=${engine}').then(function(response){return response.json();}),Promise.resolve('reopened')]).then(function(parts){
check(parts[0].ok&&parts[1]==='reopened','persisted HTTP cookies');
document.getElementById('result').textContent=JSON.stringify(parts[0]);
document.getElementById('status').textContent='Persistent HTTP and script cookies survived; HttpOnly stayed private; the session cookie ended.';
document.title='Forms persistence OK';
}).catch(fail);
}catch(error){fail(error);}`);
}

const server = http.createServer(async (request, response) => {
  try {
    const chunks = [];
    let size = 0;
    for await (const chunk of request) {
      size += chunk.length;
      if (size > 65536) throw new Error('Owned fixture request exceeded 64 KiB');
      chunks.push(chunk);
    }
    const body = Buffer.concat(chunks).toString('utf8');
    const url = new URL(request.url, origin);
    const engine = url.searchParams.get('engine') ?? 'unknown';
    assert.ok(requests.length < 512, 'Owned request log exceeded 512 records');
    const entry = { phase, engine, method: request.method, path: url.pathname, query: url.search, headers: request.headers, body };
    requests.push(entry);
    const jar = cookieMap(request.headers.cookie);
    response.setHeader('Cache-Control', 'no-store');
    if (url.pathname === '/form') {
      response.setHeader('Set-Cookie', [
        `visible=${cookies.visible}; Max-Age=3600; Path=/; SameSite=Lax`,
        `private=${cookies.private}; Max-Age=3600; HttpOnly; Path=/; SameSite=Lax`,
        `ephemeral=${cookies.ephemeral}; Path=/; SameSite=Lax`,
      ]);
      const source = formSource(engine);
      sources.set(`form-${engine}.html`, source);
      response.setHeader('Content-Type', 'text/html;charset=utf-8');
      response.end(source);
    } else if (url.pathname === '/submit') {
      const prefix = `${engine}: form submission`;
      const okay = [
        recordCheck(`${prefix} POST`, request.method === 'POST', request.method),
        recordCheck(`${prefix} exact URL-encoded body`, body === expectedBody, body),
        recordCheck(`${prefix} decoded form values`, JSON.stringify([...new URLSearchParams(body)]) === JSON.stringify(expectedEntries)),
        recordCheck(`${prefix} content type`, request.headers['content-type']?.toLowerCase().startsWith('application/x-www-form-urlencoded'), request.headers['content-type']),
        recordCheck(`${prefix} Origin`, request.headers.origin === origin, request.headers.origin),
        ...['visible', 'private', 'ephemeral', 'script'].map(name => recordCheck(`${prefix} Cookie ${name}`, jar[name] === cookies[name])),
      ].every(Boolean);
      submissions.set(engine, { ok: okay, fields: expectedEntries.map(([name]) => name), receivedBytes: Buffer.byteLength(body) });
      response.statusCode = 303;
      response.setHeader('Location', `/receipt?engine=${engine}`);
      response.setHeader('Set-Cookie', `after_post=${cookies.after_post}; Max-Age=3600; HttpOnly; Path=/; SameSite=Lax`);
      response.end('redirecting');
    } else if (url.pathname === '/receipt') {
      recordCheck(`${engine}: 303 became GET`, request.method === 'GET' && body === '', `${request.method} ${body}`);
      recordCheck(`${engine}: redirect Set-Cookie on next hop`, jar.after_post === cookies.after_post);
      const source = receiptSource(engine);
      sources.set(`receipt-${engine}.html`, source);
      response.setHeader('Content-Type', 'text/html;charset=utf-8');
      response.end(source);
    } else if (url.pathname === '/api/receipt' || url.pathname === '/api/extra') {
      const privateCookie = recordCheck(`${engine}: authenticated Fetch ${url.pathname}`, jar.private === cookies.private && jar.after_post === cookies.after_post);
      response.setHeader('Content-Type', 'application/json');
      if (url.pathname === '/api/receipt') {
        response.setHeader('Set-Cookie', `from_fetch=${cookies.from_fetch}; Max-Age=3600; HttpOnly; Path=/; SameSite=Lax`);
        response.end(JSON.stringify({ ...submissions.get(engine), ok: privateCookie && submissions.get(engine)?.ok === true }));
      } else response.end(JSON.stringify({ marker: 'second response', privateCookie }));
    } else if (url.pathname === '/reopen' || url.pathname === '/api/persist') {
      const okay = [
        ...['visible', 'private', 'script', 'after_post', 'from_fetch'].map(name => recordCheck(`${engine}: persisted ${name} at ${url.pathname}`, jar[name] === cookies[name])),
        recordCheck(`${engine}: session cookie expired at ${url.pathname}`, jar.ephemeral === undefined),
      ].every(Boolean);
      if (url.pathname === '/api/persist') {
        response.setHeader('Content-Type', 'application/json');
        response.end(JSON.stringify({ ok: okay, persisted: true, httpOnlyHidden: true, sessionEnded: true }));
      } else {
        const source = reopenSource(engine);
        sources.set(`reopen-${engine}.html`, source);
        response.setHeader('Content-Type', 'text/html;charset=utf-8');
        response.end(source);
      }
    } else {
      response.statusCode = 404;
      response.end('owned fixture: not found');
    }
  } catch (error) {
    checks.push({ name: 'fixture handler error', pass: false, detail: String(error) });
    response.statusCode = 500;
    response.end('owned fixture failed');
  }
});
await new Promise((resolveListen, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', resolveListen); });
origin = `http://127.0.0.1:${server.address().port}`;

async function run(executable, args, stem, deadline = timeoutMs) {
  const invocation = { executable, args, timeoutMs: deadline, startedAt: new Date().toISOString() };
  await writeFile(resolve(output, `${stem}-invocation.json`), JSON.stringify(invocation, null, 2));
  return new Promise((resolveRun, reject) => {
    const child = spawn(executable, args, { cwd: repo, windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
    invocation.pid = child.pid;
    child.once('spawn', () => {
      writeFile(resolve(output, `${stem}-invocation.json`), JSON.stringify(invocation, null, 2))
        .catch(error => recordCheck(`${stem}: invocation evidence saved`, false, String(error)));
    });
    let stdout = '', stderr = '', timedOut = false;
    const timer = setTimeout(() => { timedOut = true; child.kill(); }, deadline);
    child.stdout.on('data', chunk => { stdout += chunk; if (stdout.length > 32 * 1024 * 1024) child.kill(); });
    child.stderr.on('data', chunk => { stderr += chunk; if (stderr.length > 4 * 1024 * 1024) child.kill(); });
    child.once('error', error => { clearTimeout(timer); reject(error); });
    child.once('close', async (code, signal) => {
      clearTimeout(timer);
      try {
        await writeFile(resolve(output, `${stem}-stdout.json`), stdout);
        await writeFile(resolve(output, `${stem}-stderr.txt`), stderr);
        if (timedOut || code !== 0) throw new Error(`${stem}: ${timedOut ? 'deadline exceeded' : `exit ${code}/${signal}`} ${stderr.slice(-3000)}`);
        resolveRun(JSON.parse(stdout));
      } catch (error) { reject(error); }
    });
  });
}
async function screenshot(name) {
  const path = resolve(output, name);
  const bytes = await readFile(path);
  assert.ok(bytes.length > 1000, `${name} is unexpectedly small`);
  assert.equal(bytes.subarray(0, 8).toString('hex'), '89504e470d0a1a0a', `${name} is not PNG`);
  return { path, bytes: bytes.length, sha256: createHash('sha256').update(bytes).digest('hex') };
}

const actions = [
  { action: 'title', value: 'Forms ready' },
  { action: 'click', id: 'username' },
  { action: 'key', value: 'a', control: true },
  { action: 'text', value: values.username },
  { action: 'key', value: 'Tab' },
  { action: 'text', value: values.password },
  { action: 'click', id: 'subscribe' },
  { action: 'click', id: 'radio_b' },
  { action: 'click', id: 'plan' },
  { action: 'key', value: 'ArrowDown' },
  { action: 'key', value: 'Enter' },
  { action: 'click', id: 'note' },
  { action: 'key', value: 'a', control: true },
  { action: 'text', value: values.note },
  { action: 'click', id: 'submit' },
  { action: 'title', value: 'Forms pipeline OK' },
];
const report = { runId, binary, profile, origin, mode: chromeOnly ? 'chrome-reference-only' : 'rust-with-optional-reference', valuesAreSynthetic: true, native: null, reopen: null, chrome: null, checks, requests, screenshots: [] };
try {
  await writeFile(resolve(output, 'expected-form.json'), JSON.stringify({ entries: expectedEntries, encodedBody: expectedBody }, null, 2));
  await writeFile(resolve(output, 'native-actions.json'), JSON.stringify(actions, null, 2));
  await writeFile(resolve(output, 'reopen-actions.json'), JSON.stringify([{ action: 'title', value: 'Forms persistence OK' }], null, 2));
  if (!chromeOnly) {
    report.native = await run(binary, ['--smoke-window', resolve(output, 'native-form.png'), '--smoke-actions', resolve(output, 'native-actions.json'), '--url', `${origin}/form?engine=rust`, '--profile', profile], 'native');
    assert.equal(report.native.title, 'Forms pipeline OK');
    assert.equal(report.native.url, `${origin}/receipt?engine=rust`);
    assert.ok(report.native.window_smoke && report.native.frames_presented > 0);
    report.screenshots.push(await screenshot('native-form.png'));
    const cookieFile = await readFile(resolve(profile, 'Cookies.v1.dpapi'));
    recordCheck('persistent cookie file written', cookieFile.length > 32);
    recordCheck('persistent HttpOnly bytes protected at rest', !cookieFile.includes(Buffer.from(cookies.private)));
    report.cookieFile = { bytes: cookieFile.length, sha256: createHash('sha256').update(cookieFile).digest('hex') };
    phase = 'rust-reopen';
    report.reopen = await run(binary, ['--smoke-window', resolve(output, 'native-reopen.png'), '--smoke-actions', resolve(output, 'reopen-actions.json'), '--url', `${origin}/reopen?engine=rust`, '--profile', profile], 'reopen');
    assert.equal(report.reopen.title, 'Forms persistence OK');
    assert.ok(report.reopen.window_smoke && report.reopen.frames_presented > 0);
    report.screenshots.push(await screenshot('native-reopen.png'));
  }
  if (options.get('--chrome-reference')) {
    phase = 'chrome-reference';
    const locator = selector => ({ kind: 'css', selector });
    const step = (step_id, action, rest) => ({ step_id, action, timeout_ms: 10000, ...rest });
    const literal = value => ({ source: 'literal', value });
    const scenario = {
      browser: { mode: 'launch', executable_path: options.get('--chrome') ?? 'C:/Program Files/Google/Chrome/Application/chrome.exe', headless: true },
      start_url: { url: `${origin}/form?engine=chrome` },
      actions: [
        step('ready', 'wait_for', { locator: locator('#status'), state: 'visible' }),
        step('username', 'fill', { locator: locator('#username'), value: literal(values.username) }),
        step('password', 'fill', { locator: locator('#password'), value: literal(values.password) }),
        step('subscribe', 'check', { locator: locator('#subscribe') }),
        step('radio', 'check', { locator: locator('#radio_b') }),
        step('plan', 'select_option', { locator: locator('#plan'), value: literal('beta') }),
        step('note', 'fill', { locator: locator('#note'), value: literal(values.note) }),
        step('submit', 'click', { locator: locator('#submit') }),
        step('complete', 'wait_for', { locator: locator('title'), state: 'attached' }),
        step('verified-result', 'wait_for', { locator: locator('#result[data-ready="yes"]'), state: 'attached' }),
      ],
      capture: { at_end: ['url', 'dom'], events: ['page-errors', 'network'], network: { request_body: true, response_body: true, header_values: true } },
      command_timeout_ms: 45000,
    };
    // The receipt sets its data-ready marker only after page checks finish.
    const path = resolve(output, 'chrome-scenario.json');
    await writeFile(path, JSON.stringify(scenario, null, 2));
    const rea = resolve(options.get('--rea') ?? 'C:/Users/admin/AppData/Local/npm-cache/_npx/da7caeae9db9c589/node_modules/rea-agents/scripts/rea.mjs');
    report.chrome = await run(process.execPath, [rea, 'capture-browser-scenario', path, '--format', 'json', '--full-output'], 'chrome-rea', Math.max(timeoutMs, 90000));
    const capture = report.chrome.data?.normalized_result;
    assert.equal(report.chrome.ok, true, 'REA did not return a successful tool result');
    assert.ok(capture?.steps?.length > 0, 'REA returned no scenario steps');
    recordCheck('Chrome REA scenario steps completed', capture.steps.every(step => step.status === 'completed'));
    recordCheck('Chrome owned process cleaned up', capture.browser.process_ownership === 'provider-owned' && capture.browser.cleanup === 'terminated-owned-process');
    const last = capture.steps.at(-1);
    const dom = last.artifacts.dom;
    recordCheck('Chrome final receipt URL', last.after_url.url === `${origin}/receipt?engine=chrome`);
    recordCheck('Chrome final verified DOM', dom.state === 'captured' && dom.value.text.includes('<title>Forms pipeline OK</title>') && dom.value.text.includes('data-ready="yes"'));
    if (dom.state === 'captured') await writeFile(resolve(output, 'chrome-final.html'), dom.value.text);
    report.chromeSummary = {
      evidenceId: report.chrome.data.evidence_id, browser: capture.browser,
      steps: capture.steps.map(step => ({ id: step.step_id, status: step.status })),
      completeness: capture.completeness, limitations: capture.limitations,
    };
    await writeFile(resolve(output, 'chrome-summary.json'), JSON.stringify(report.chromeSummary, null, 2));
    recordCheck('Chrome submitted the identical encoded form', submissions.get('chrome')?.ok === true);
  }
  if (!chromeOnly) {
    assert.ok(requests.some(request => request.phase === 'rust-initial' && request.path === '/submit'), 'Native submit did not reach HTTP');
    assert.ok(requests.some(request => request.phase === 'rust-reopen' && request.path === '/api/persist'), 'Reopen did not perform authenticated Fetch');
  }
  assert.deepEqual(checks.filter(check => !check.pass), [], 'One or more independent HTTP checks failed');
  report.ok = true;
} catch (error) {
  report.ok = false;
  report.error = String(error.stack ?? error);
  process.exitCode = 1;
} finally {
  for (const [name, source] of sources) await writeFile(resolve(output, name), source);
  await writeFile(resolve(output, 'requests.json'), JSON.stringify(requests, null, 2));
  await writeFile(resolve(output, 'report.json'), JSON.stringify(report, null, 2));
  server.closeAllConnections();
  await new Promise(resolveClose => server.close(resolveClose));
}
process.stdout.write(`${JSON.stringify({ ok: report.ok, report: resolve(output, 'report.json'), nativeTitle: report.native?.title, reopenTitle: report.reopen?.title, checks: checks.length, failures: checks.filter(check => !check.pass), error: report.error })}\n`);
