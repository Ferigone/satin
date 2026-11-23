import { execFile } from 'node:child_process';
import { createHash } from 'node:crypto';
import { once } from 'node:events';
import { mkdir, mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';

const [browser, executable] = process.argv.slice(2);
if (!['chrome', 'satin'].includes(browser) || !executable || process.argv.length !== 4) {
  process.stderr.write('Usage: node tools/fixtures/chat/run.mjs <chrome|satin> <browser executable>\n');
  process.exit(2);
}

const page = await readFile(new URL('./index.html', import.meta.url), 'utf8');
const root = fileURLToPath(new URL('../../../target/chat-fixture/', import.meta.url));
await mkdir(root, { recursive: true });
const output = await mkdtemp(join(root, browser + '-'));
const profile = join(output, 'profile');
const domPath = join(output, 'dom.html');
const temp = join(output, 'temp');
await mkdir(temp);

const message = 'Fixture system message: channel ready.';
const payload = Buffer.from(JSON.stringify({ type: 'system', text: message }));
if (payload.length > 125) throw new Error('Fixture text frame exceeds the short length form');
const frame = Buffer.concat([Buffer.from([0x81, payload.length]), payload]);
const transport = { pageRequests: 0, upgradeAttempts: 0, upgradesAccepted: 0, textFramesWritten: 0 };
const sockets = new Set();
const server = createServer((request, response) => {
  if (request.method === 'GET' && request.url === '/') {
    transport.pageRequests++;
    response.writeHead(200, {
      'Content-Type': 'text/html; charset=utf-8',
      'Cache-Control': 'no-store',
    }).end(page);
  } else {
    response.writeHead(request.url === '/favicon.ico' ? 204 : 404).end();
  }
});
server.on('connection', socket => {
  sockets.add(socket);
  socket.on('close', () => sockets.delete(socket));
});
server.on('upgrade', (request, socket) => {
  transport.upgradeAttempts++;
  socket.on('error', () => socket.destroy());
  const key = request.headers['sec-websocket-key'];
  if (request.method !== 'GET' || request.url !== '/delivery' ||
      request.headers.origin !== origin || request.headers.upgrade?.toLowerCase() !== 'websocket' ||
      request.headers['sec-websocket-version'] !== '13' ||
      typeof key !== 'string' || !/^[A-Za-z0-9+/]{22}==$/.test(key)) {
    socket.end('HTTP/1.1 400 Bad Request\r\nConnection: close\r\nContent-Length: 0\r\n\r\n');
    return;
  }
  const accept = createHash('sha1').update(key + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').digest('base64');
  const response = 'HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n' +
    'Connection: Upgrade\r\nSec-WebSocket-Accept: ' + accept + '\r\n\r\n';
  transport.upgradesAccepted++;
  socket.write(Buffer.concat([Buffer.from(response), frame]), error => {
    if (!error) transport.textFramesWritten++;
  });
});
server.listen(0, '127.0.0.1');
await once(server, 'listening');
const origin = 'http://127.0.0.1:' + server.address().port;

const report = {
  browser, version: null, delivered: false, checks: null,
  firstError: null, harnessError: null, transport,
};
const satin = browser === 'satin';
try {
  const args = satin ? [
    '--headless', '--url', origin + '/', '--profile', profile,
    '--run-for-ms', '3500', '--dump-dom', domPath,
  ] : [
    '--headless', '--user-data-dir=' + profile, '--no-first-run', '--no-default-browser-check',
    '--disable-background-networking', '--disable-component-update', '--disable-sync',
    '--no-proxy-server', '--host-resolver-rules=MAP * ~NOTFOUND, EXCLUDE 127.0.0.1',
    '--virtual-time-budget=5000', '--dump-dom', origin + '/',
  ];
  const { stdout } = await promisify(execFile)(resolve(executable), args, {
    cwd: output, env: { ...process.env, TEMP: temp, TMP: temp },
    windowsHide: true, timeout: 25_000, maxBuffer: 1024 * 1024,
  });
  const browserReport = satin ? JSON.parse(stdout) : null;
  report.version = browserReport?.version ?? null;
  const dom = satin ? await readFile(domPath, 'utf8') : stdout;
  if (!satin) await writeFile(domPath, dom);

  const receipt = /<pre id="checks">(\{[^<]+\})<\/pre>/.exec(dom);
  const error = /<pre id="first-error">([^<]*)<\/pre>/.exec(dom);
  if (!receipt || !error) throw new Error('Missing page receipt in the DOM dump');
  report.checks = JSON.parse(receipt[1]);
  report.firstError = error[1] || null;
  const rows = Array.from(dom.matchAll(/<p id="system-message">([^<]*)<\/p>/g), m => m[1]);
  report.delivered = report.firstError === null && report.checks.rendered === true &&
    Object.values(report.checks).every(value => value === true) &&
    rows.length === 1 && rows[0] === message &&
    transport.pageRequests === 1 && transport.upgradeAttempts === 1 &&
    transport.upgradesAccepted === 1 && transport.textFramesWritten === 1;
  if (!report.delivered && !report.firstError) {
    throw new Error('Incomplete or inconsistent page/transport receipt');
  }
} catch (error) {
  report.harnessError = error.killed ? 'Browser process timed out' : (error.code ?? error.message);
} finally {
  const closed = once(server, 'close');
  server.close();
  for (const socket of sockets) socket.destroy();
  await closed;
}

const receipt = JSON.stringify({ ...report, output }, null, 2) + '\n';
await writeFile(join(output, 'report.json'), receipt);
process.stdout.write(receipt);
process.exitCode = report.delivered ? 0 : 1;
