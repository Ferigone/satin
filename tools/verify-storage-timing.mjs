import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { once } from 'node:events';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { resolve } from 'node:path';

const positional = process.argv.slice(2).filter(value => !value.startsWith('--'));
const chromeOnly = process.argv.includes('--chrome-reference-only');
const withChrome = chromeOnly || process.argv.includes('--chrome-reference');
const binary = resolve(positional[0] ?? 'target/debug/rust-browser.exe');
const output = resolve(positional[1] ?? 'target/storage-timing-verification');
const rea = resolve(process.env.REA_CLI ?? 'node_modules/rea-agents/scripts/rea.mjs');
const chrome = resolve(process.env.ProgramFiles ?? 'C:/Program Files', 'Google/Chrome/Application/chrome.exe');
const fixture = await readFile(new URL('./fixtures/storage-timing.html', import.meta.url));
const digest = bytes => createHash('sha256').update(bytes).digest('hex');
const runId = randomUUID();
await mkdir(output, { recursive:true });
const json = (name, content) => writeFile(resolve(output, name), JSON.stringify(content, null, 2) + '\n');

const writer = {named:true, separateTab:true, timing:[
  [17,'tick1,micro,tick2,micro,tick3,micro'],
  ['first,micro,second,next',true,true,true],
]};
const listener = {events:[[null,'one'],['one','two'],['two',null]], scoped:true};
const reloaded = {saved:'Łódź 🦀',raw:55296,session:'writer-origin'};
const reopened = {...reloaded,session:null};
const collisions = {
  rust:['function','function','shadow',false,'function','shadow'],
  chrome:['function','string','method',true,'function','method'],
};
const expected = {
  '/writer-receipt':writer, '/listener-receipt':listener,
  '/reload-receipt':reloaded, '/reopen-receipt':reopened,
};
const states = new Map(['headless','native','chrome'].map(mode => [mode, {
  receipts:{}, gate:null, finished:false,
}]));
const report = {
  ok:false, fixtureSha256:digest(fixture),
  binarySha256:chromeOnly ? null : digest(await readFile(binary)),
  expected, collisions, requests:[], runs:{}, failures:[],
};

function reply(response, status, contentType, body = '') {
  response.writeHead(status, {'Content-Type':contentType, 'Cache-Control':'no-store'});
  response.end(body);
}
function release(state, mode) {
  if (!state.gate || state.finished || !state.receipts['/writer-receipt'] ||
      mode !== 'headless' && !state.receipts['/listener-receipt']) return;
  state.finished = true;
  reply(state.gate, 204, 'text/plain');
}
let origin;
async function serve(request, response) {
  const address = new URL(request.url, origin);
  const mode = address.searchParams.get('mode');
  const state = states.get(mode);
  const route = address.pathname;
  if (!state) return reply(response,404,'text/plain','unknown mode');
  report.requests.push({mode, route, method:request.method});
  if (['/listener','/writer','/reload','/reopen'].includes(route)) {
    assert.equal(request.method,'GET');
    return reply(response,200,'text/html; charset=utf-8',fixture);
  }
  if (route === '/gate') {
    assert.equal(request.method,'GET');
    assert.equal(state.gate,null,'duplicate network latch');
    state.gate = response;
    release(state,mode);
    return;
  }
  if (Object.hasOwn(expected,route) || route === '/diagnostic') {
    assert.equal(request.method,'POST');
    assert.equal(state.receipts[route],undefined,'duplicate receipt');
    const buffers = [];
    let total = 0;
    for await (const buffer of request) {
      total += buffer.length;
      assert.ok(total < 8192);
      buffers.push(buffer);
    }
    const received = JSON.parse(Buffer.concat(buffers));
    if (route === '/diagnostic') {
      report.failures.push(mode + ' page: ' + JSON.stringify(received));
      if (state.gate && !state.finished) {
        state.finished = true;
        reply(state.gate,500,'text/plain','fixture diagnostic');
      }
      return reply(response,204,'text/plain');
    }
    if (route === '/writer-receipt') {
      const {collision, ...shared} = received;
      assert.deepEqual(collision,collisions[mode === 'chrome' ? 'chrome' : 'rust']);
      assert.deepEqual(shared,expected[route],mode + ' ' + route);
    } else {
      assert.deepEqual(received,expected[route],mode + ' ' + route);
    }
    state.receipts[route] = received;
    reply(response,204,'text/plain');
    release(state,mode);
    return;
  }
  reply(response,404,'text/plain','unknown role');
}
const server = createServer((request,response) => {
  serve(request,response).catch(error => {
    report.failures.push(String(error));
    if (!response.headersSent) reply(response,500,'text/plain',String(error));
    else response.destroy();
  });
});
server.listen(0,'127.0.0.1');
await once(server,'listening');
origin = 'http://127.0.0.1:' + server.address().port;
const url = (route,mode) => origin + route + '?mode=' + mode;

async function processJson(executable, args, name, timeout = 35_000) {
  const child = spawn(executable,args,{windowsHide:true,stdio:['ignore','pipe','pipe']});
  const stdout = [], stderr = [];
  child.stdout.on('data',data => stdout.push(data));
  child.stderr.on('data',data => stderr.push(data));
  const alarm = setTimeout(() => child.kill(),timeout);
  try {
    const [exit,signal] = await once(child,'close');
    const out = Buffer.concat(stdout).toString();
    const err = Buffer.concat(stderr).toString();
    await writeFile(resolve(output,name + '.stdout.txt'),out);
    if (err) await writeFile(resolve(output,name + '.stderr.txt'),err);
    assert.equal(exit,0, name + ' ' + (signal ?? '') + ' ' + err + ' ' + out.slice(-1000));
    return JSON.parse(out);
  } finally {
    clearTimeout(alarm);
  }
}

function surface(result, expectedTitle) {
  assert.equal(result.title,expectedTitle,JSON.stringify(result));
  assert.equal(result.layout_limited,false);
  assert.equal(result.paint_limited,false);
  assert.deepEqual(result.javascript.diagnostics,[]);
}

function observed(mode, names) {
  const state = states.get(mode);
  assert.deepEqual(Object.keys(state.receipts).sort(),names.slice().sort());
  return state.receipts;
}

async function headless() {
  const profile = resolve(output,'headless-' + runId);
  const initial = await processJson(binary,['--headless','--url',url('/writer','headless'),
    '--run-for-ms','5000','--profile',profile,'--dump-dom',resolve(output,'headless.html')],'headless');
  surface(initial,'Storage reload OK');
  const resumed = await processJson(binary,['--headless','--url',url('/reopen','headless'),
    '--profile',profile],'headless-reopen');
  surface(resumed,'Storage reopened OK');
  report.runs.headless = {initial,resumed,receipts:observed('headless',
    ['/writer-receipt','/reload-receipt','/reopen-receipt'])};
}

async function native() {
  const profile = resolve(output,'native-' + runId);
  const actions = [
    {action:'title',value:'Storage listener ready'},
    {action:'key',value:'t',control:true},
    {action:'text',value:url('/writer','native')},
    {action:'key',value:'Enter'},
    {action:'title',value:'Storage writer OK'},
    {action:'key',value:'Tab',control:true},
    {action:'title',value:'Storage listener OK'},
  ];
  await json('smoke-actions.json',actions);
  const result = await processJson(binary,['--url',url('/listener','native'),'--profile',profile,
    '--smoke-window',resolve(output,'native.png'),'--smoke-actions',resolve(output,'smoke-actions.json')],'native');
  surface(result,'Storage listener OK');
  assert.ok(result.window_smoke && result.frames_presented >= 1);
  const fresh = await processJson(binary,['--headless','--url',url('/reopen','native'),
    '--profile',profile],'native-reopen');
  surface(fresh,'Storage reopened OK');
  report.runs.native = {result,fresh,receipts:observed('native',
    ['/writer-receipt','/listener-receipt','/reopen-receipt'])};
}

async function chromium() {
  const ok = '#outcome[data-complete="yes"]';
  await json('chrome-scenario.json',{
    browser:{mode:'launch',executable_path:chrome,headless:true},
    start_url:{url:url('/listener','chrome')},
    actions:[
      {step_id:'open-writer',action:'click',locator:{kind:'css',selector:'#open-writer'}},
      {step_id:'listener-receipt',action:'wait_for',state:'attached',timeout_ms:10_000,
        locator:{kind:'css',selector:ok}},
    ],
    capture:{at_end:['url','dom'],events:['page-errors','network']},
    command_timeout_ms:22_000,
  });
  const captured = await processJson(process.execPath,[rea,'capture-browser-scenario',
    resolve(output,'chrome-scenario.json'),'--format','json','--full-output'],'chrome');
  assert.equal(captured.ok,true);
  const result = captured.data.normalized_result;
  assert.equal(result.browser.cleanup,'terminated-owned-process');
  const dom = result.steps.at(-1).artifacts.dom;
  assert.equal(dom.state,'captured');
  assert.match(dom.value.text,/<title>Storage listener OK<\/title>/);
  report.runs.chrome = {evidenceId:captured.data.evidence_id,browser:result.browser,
    receipts:observed('chrome',['/writer-receipt','/listener-receipt'])};
}

for (const [name, run] of [
  ...(!chromeOnly ? [['headless',headless],['native',native]] : []),
  ...(withChrome ? [['chrome',chromium]] : []),
]) {
  try {
    await run();
  } catch (error) {
    report.failures.push(name + ': ' + String(error));
  }
}
server.closeAllConnections();
server.close();
report.ok = report.failures.length === 0;
await json('report.json',report);
console.log(JSON.stringify({ok:report.ok, modes:Object.keys(report.runs), failures:report.failures,
  collisions:Object.fromEntries([...states].map(([mode,state]) =>
    [mode,state.receipts['/writer-receipt']?.collision]).filter(([,value]) => value)),
  requests:report.requests.length, report:resolve(output,'report.json')}));
if (!report.ok) process.exitCode = 1;
