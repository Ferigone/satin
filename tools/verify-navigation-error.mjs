#!/usr/bin/env node

import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { once } from 'node:events';
import { mkdir, mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { createConnection, createServer as createTcpServer } from 'node:net';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';

const [first, second] = process.argv.slice(2);
const current = first === '--baseline-only' ? null : first;
const baseline = second;
assert.ok(current || baseline,
  'Usage: node tools/verify-navigation-error.mjs <satin.exe> [<baseline.exe>] or --baseline-only <baseline.exe>');
const project = fileURLToPath(new URL('../', import.meta.url));
const root = join(project, 'target', 'verification-navigation-error');
await mkdir(root, { recursive: true });
const output = await mkdtemp(join(root, 'run-'));
const run = promisify(execFile);
const failureTitle = 'Page could not load';
const recoveredTitle = 'Loopback recovery OK';
const host = '127.0.0.1';
// Keep HTTP local and deterministic, even on machines configured with a proxy.
const env = { ...process.env };
for (const key of Object.keys(env)) {
  if (/^(all|https?|no)_proxy$/i.test(key)) delete env[key];
}
env.NO_PROXY = host;

async function listen(server) {
  server.listen(0, host);
  await once(server, 'listening');
  return server.address().port;
}

async function close(server) {
  await new Promise((done, reject) => server.close(error => error ? reject(error) : done()));
}

async function windowSmoke(executable, caseName, url, title, actions = []) {
  const image = join(output, `${caseName}.png`);
  const args = [
    '--url', url, '--profile', join(output, `${caseName}-profile`),
    '--smoke-window', image, '--width', '1000', '--height', '600',
  ];
  if (actions.length) {
    const actionFile = join(output, `${caseName}-actions.json`);
    await writeFile(actionFile, JSON.stringify(actions));
    args.push('--smoke-actions', actionFile);
  }
  const { stdout } = await run(resolve(executable), args, {
    cwd: project, windowsHide: true, timeout: 20_000, env,
  });
  const report = JSON.parse(stdout);
  assert.equal(report.window_smoke, true);
  assert.ok(report.frames_presented > 0);
  assert.equal(report.title, title);
  assert.equal(report.url, url);
  assert.equal(report.layout_limited, false);
  const png = await readFile(image);
  assert.equal(png.subarray(0, 8).toString('hex'), '89504e470d0a1a0a');
  return { version: report.version, title, url, frames: report.frames_presented, capture: `${caseName}.png` };
}

// Ask the OS for an unused port, release it, then confirm the connection is refused.
const reserve = createTcpServer();
const closedPort = await listen(reserve);
await close(reserve);
const connection = createConnection({ host, port: closedPort });
connection.on('connect', () => connection.destroy(new Error('Closed port accepted a connection')));
const [refusal] = await once(connection, 'error');
assert.equal(refusal.code, 'ECONNREFUSED');
const unreachable = `http://${host}:${closedPort}/unreachable`;
const report = {};
if (baseline) {
  report.before = await windowSmoke(baseline, 'before-closed', unreachable,
    'Nie uda\u0142o si\u0119 otworzy\u0107 strony');
}
report.closedPortCheck = refusal.code;

// Keep the same address reserved across both attempts. Drop the initial HTTP
// response before headers; the next request can then recover through the real UI.
if (current) {
  report.after = await windowSmoke(current, 'after-closed', unreachable, failureTitle);
  const requests = [];
  const route = '/retry?one=1&two=2';
  const fixture = createServer((request, response) => {
    requests.push({ method: request.method, route: request.url });
    if (requests.length === 1) {
      request.socket.destroy();
      return;
    }
    response.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8', Connection: 'close' });
    response.end(`<!doctype html><meta charset="utf-8"><title>${recoveredTitle}</title>
      <main><h1>Loopback fixture restored</h1><p>The error page retried this URL.</p>
      <p>Navigation returned to an ordinary document.</p><p>End of fixture.</p></main>`);
  });
  const livePort = await listen(fixture);
  try {
    report.recovered = await windowSmoke(current, 'after-retry',
      `http://${host}:${livePort}${route}`, recoveredTitle, [
        { action: 'title', value: failureTitle },
        { action: 'click', id: 'satin-retry' },
        { action: 'wait_title', value: recoveredTitle, timeout_ms: 5000 },
      ]);
    assert.deepEqual(requests, [{ method: 'GET', route }, { method: 'GET', route }]);
    report.requests = requests;
  } finally {
    fixture.closeAllConnections();
    await close(fixture);
  }
}
await writeFile(join(output, 'report.json'), JSON.stringify(report, null, 2));
console.log(output);
console.log(JSON.stringify(report, null, 2));
