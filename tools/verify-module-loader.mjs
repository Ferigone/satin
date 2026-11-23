import { isDeepStrictEqual, parseArgs } from 'node:util';
import { spawn } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { once } from 'node:events';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { fileURLToPath } from 'node:url';

const { values: options } = parseArgs({ options: {
  chrome: { type: 'string' },
  satin: { type: 'string' },
  output: { type: 'string' },
} });
if (!options.chrome && !options.satin) {
  throw new Error('Usage: node tools/verify-module-loader.mjs [--chrome EXE] [--satin EXE] [--output DIRECTORY]');
}

const root = fileURLToPath(new URL('..', import.meta.url));
const fixture = new URL('./fixtures/module-loader/', import.meta.url);
const output = resolve(options.output ?? resolve(root, `target/module-loader-${randomUUID()}`));
await mkdir(output, { recursive: true });

const expected = {
  scoped: ['deep scope', 'outer scope fallback', 'global prefix fallback'],
  unscoped: 'unscoped import',
  cache: {
    distinctPromises: true,
    sameNamespace: true,
    sameExport: true,
    evaluations: 1,
    liveBinding: [0, 1, 1],
  },
  blocked: 'rejected:TypeError',
  invalidAddress: 'rejected:TypeError',
};
const expectedFiles = [
  'index.html', 'main.mjs', 'modules/deep/a.mjs', 'outside/b.mjs', 'modules/shared.mjs',
  'targets/deep.mjs', 'targets/global-fallback.mjs', 'targets/global.mjs', 'targets/outer-fallback.mjs',
];
const unexpectedFiles = ['targets/outer.mjs', 'would-be-relative.mjs'];
const files = new Map(await Promise.all(expectedFiles.concat(unexpectedFiles).map(async name =>
  [name, await readFile(new URL(name, fixture))])));
const requests = { chrome: [], satin: [] };
const server = createServer((request, response) => {
  const match = /^\/(chrome|satin)\/(.+)$/.exec(request.url);
  const [, mode, name] = match ?? [];
  const source = request.method === 'GET' && files.get(name);
  const status = source ? 200 : 404;
  if (mode) requests[mode].push({ method: request.method, name, status });
  response.writeHead(status, {
    'Content-Type': !source ? 'text/plain; charset=utf-8'
      : name.endsWith('.mjs') ? 'text/javascript; charset=utf-8' : 'text/html; charset=utf-8',
    'Cache-Control': 'no-store',
  });
  response.end(source || 'Not found');
});

const sha256 = data => createHash('sha256').update(data).digest('hex');
const write = (name, data) => writeFile(resolve(output, name), data);
const profile = mode => resolve(output, `${mode}-profile-${randomUUID()}`);
const report = {
  expected,
  expectedRequests: expectedFiles.slice().sort(),
  fixtureSha256: Object.fromEntries([...files].map(([name, bytes]) => [name, sha256(bytes)])),
  runs: {},
};

function launch(executable, args, label) {
  const child = spawn(executable, args, { cwd: root, windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
  let stdout = '';
  let stderr = '';
  child.stdout.setEncoding('utf8').on('data', chunk => { stdout += chunk; });
  child.stderr.setEncoding('utf8').on('data', chunk => { stderr += chunk; });
  const completed = once(child, 'close').then(([code, signal]) => ({ code, signal }), error =>
    ({ error: error.message })).then(async result => {
    await Promise.all([write(`${label}.stdout.txt`, stdout), write(`${label}.stderr.txt`, stderr)]);
    return { ...result, stdout, stderr };
  });
  return { child, completed };
}

function requireExit(processResult) {
  if (processResult.code !== 0) {
    throw new Error(processResult.error ?? `Exit ${processResult.code ?? processResult.signal}: ${processResult.stderr.slice(-1000)}`);
  }
}

async function satin(executable, entry) {
  const version = launch(executable, ['--version'], 'satin-version');
  const versionTimer = setTimeout(() => version.child.kill(), 10000);
  try {
    const info = await version.completed;
    requireExit(info);
    entry.version = info.stdout.trim();
  } finally {
    clearTimeout(versionTimer);
  }
  const path = resolve(output, 'satin.html');
  entry.args = [
    '--headless', '--url', entry.url, '--profile', profile('satin'),
    '--run-for-ms', '5000', '--dump-dom', path,
  ];
  const { child, completed } = launch(executable, entry.args, 'satin');
  const timer = setTimeout(() => child.kill(), 25000);
  try {
    const result = await completed;
    requireExit(result);
    entry.browserResult = JSON.parse(result.stdout);
    return await readFile(path, 'utf8');
  } finally {
    clearTimeout(timer);
  }
}

let commandId = 0;
function cdp(socket, method, params, sessionId) {
  return new Promise((done, reject) => {
    const id = ++commandId;
    const timeout = setTimeout(() => {
      socket.removeEventListener('message', receive);
      reject(new Error(`Chrome command timed out: ${method}`));
    }, 3000);
    function receive(event) {
      const message = JSON.parse(event.data);
      if (message.id !== id) return;
      clearTimeout(timeout);
      socket.removeEventListener('message', receive);
      if (message.error) reject(new Error(`${method}: ${message.error.message}`));
      else done(message.result);
    }
    socket.addEventListener('message', receive);
    socket.send(JSON.stringify({ id, method, params, sessionId }));
  });
}

async function chrome(executable, entry) {
  const directory = profile('chrome');
  entry.args = [
    '--headless=new', '--no-first-run', '--no-default-browser-check', '--disable-background-networking',
    '--disable-component-update', '--remote-debugging-port=0', `--user-data-dir=${directory}`, 'about:blank',
  ];
  const { child, completed } = launch(executable, entry.args, 'chrome');
  let socket;
  try {
    let port;
    for (let attempt = 0; attempt < 100 && !port; attempt++) {
      const endpoint = await readFile(resolve(directory, 'DevToolsActivePort'), 'utf8').catch(() => '');
      port = endpoint.match(/^(\d+)\r?\n/)?.[1];
      if (!port) {
        if (child.exitCode !== null || !child.pid) {
          requireExit(await completed);
          throw new Error('Chrome exited before opening its DevTools endpoint');
        }
        await delay(50);
      }
    }
    if (!port) throw new Error('Chrome did not open its DevTools endpoint');
    const endpoint = await fetch(`http://127.0.0.1:${port}/json/version`, {
      signal: AbortSignal.timeout(3000),
    }).then(response => response.json());
    socket = new WebSocket(endpoint.webSocketDebuggerUrl);
    await once(socket, 'open', { signal: AbortSignal.timeout(3000) });
    entry.version = await cdp(socket, 'Browser.getVersion');

    const { targetInfos } = await cdp(socket, 'Target.getTargets');
    const tab = targetInfos.find(target => target.type === 'page' && target.url === 'about:blank');
    if (!tab) throw new Error('Chrome did not open its blank tab');
    const { sessionId } = await cdp(socket, 'Target.attachToTarget', { targetId: tab.targetId, flatten: true });
    const navigation = await cdp(socket, 'Page.navigate', { url: entry.url }, sessionId);
    if (navigation.errorText) throw new Error(navigation.errorText);

    // Wait for the fixture's own completion marker, not a fixed load delay.
    const expression = `location.href === ${JSON.stringify(entry.url)} && Boolean(document.querySelector('#module-loader-receipt[data-complete="yes"]'))`;
    for (let attempt = 0; attempt < 100; attempt++) {
      const { result } = await cdp(socket, 'Runtime.evaluate', { expression, returnByValue: true }, sessionId);
      if (result.value === true) break;
      await delay(50);
    }
    const { result } = await cdp(socket, 'Runtime.evaluate', {
      expression: 'document.documentElement.outerHTML', returnByValue: true,
    }, sessionId);
    await write('chrome.html', result.value);
    return result.value;
  } finally {
    if (socket?.readyState === WebSocket.OPEN) {
      socket.send(JSON.stringify({ id: ++commandId, method: 'Browser.close' }));
      await Promise.race([completed, delay(2500, undefined, { ref: false })]);
    }
    if (child.exitCode === null && child.signalCode === null) child.kill();
    socket?.close();
    const { code, signal, error } = await completed;
    entry.exit = { code, signal, error };
  }
}

function verify(mode, entry, dom) {
  const match = /<pre\b(?=[^>]*\bid="module-loader-receipt")([^>]*)>([^<]*)<\/pre>/.exec(dom);
  entry.complete = Boolean(match?.[1].includes('data-complete="yes"'));
  entry.observed = match ? JSON.parse(match[2]) : null;
  const served = requests[mode].filter(({ name }) => name !== 'favicon.ico');
  const requestedFiles = served.map(({ name }) => name).sort();
  entry.checks = {
    complete: entry.complete,
    ...Object.fromEntries(Object.entries(expected).map(([key, value]) =>
      [key, isDeepStrictEqual(entry.observed?.[key], value)])),
    noUnexpectedObservations: Object.keys(entry.observed ?? {}).every(key => Object.hasOwn(expected, key)),
    sharedFetchedOnce: served.filter(({ name, status }) => name === 'modules/shared.mjs' && status === 200).length === 1,
    resources: isDeepStrictEqual(requestedFiles, report.expectedRequests) &&
      served.every(({ method, status }) => method === 'GET' && status === 200),
  };
  entry.ok = Object.values(entry.checks).every(Boolean);
}

async function verifyBrowser(mode, run, origin) {
  const executable = resolve(options[mode]);
  const entry = report.runs[mode] = { executable, url: `${origin}/${mode}/index.html`, ok: false };
  try {
    entry.sha256 = sha256(await readFile(executable));
    verify(mode, entry, await run(executable, entry));
    if (sha256(await readFile(executable)) !== entry.sha256) {
      throw new Error('Browser executable changed during verification');
    }
  } catch (error) {
    entry.error = error.stack ?? String(error);
    entry.ok = false;
  } finally {
    entry.requests = requests[mode];
  }
}

server.listen(0, '127.0.0.1');
await once(server, 'listening');
try {
  const origin = `http://127.0.0.1:${server.address().port}`;
  if (options.chrome) await verifyBrowser('chrome', chrome, origin);
  if (options.satin) await verifyBrowser('satin', satin, origin);
} finally {
  const closed = once(server, 'close');
  server.close();
  server.closeAllConnections();
  await closed;
}
report.ok = Object.values(report.runs).every(entry => entry.ok);
await write('report.json', JSON.stringify(report, null, 2) + '\n');
console.log(JSON.stringify({
  ok: report.ok,
  report: resolve(output, 'report.json'),
  runs: Object.fromEntries(Object.entries(report.runs).map(([mode, entry]) => [mode, entry.checks ?? entry.error])),
}));
if (!report.ok) process.exitCode = 1;
