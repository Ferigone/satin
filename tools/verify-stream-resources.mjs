// Exercise the browser's streaming loader against gated loopback responses.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { once } from 'node:events';
import { readFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

assert.ok(process.argv.length <= 3, 'Usage: node tools/verify-stream-resources.mjs [BROWSER.exe]');
const binary = process.argv[2] ? resolve(process.argv[2])
  : fileURLToPath(new URL('../target/debug/rust-browser.exe', import.meta.url));
const binarySha256 = createHash('sha256').update(await readFile(binary)).digest('hex');
const png = await readFile(new URL('../assets/browser.png', import.meta.url));
const html = 'text/html; charset=utf-8';
const css = 'text/css; charset=utf-8';
const js = 'text/javascript; charset=utf-8';

function reply(response, body, type = 'text/plain', status = 200, headers = {}) {
  response.writeHead(status, { 'Content-Type': type, 'Cache-Control': 'no-store', ...headers });
  response.end(body);
}

function runBrowser(url, rejectNavigation) {
  return new Promise((resolve, reject) => {
    const child = spawn(binary, ['--headless', '--url', url, '--run-for-ms', '1500'], {
      windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'],
    });
    let stdout = '';
    let stderr = '';
    let timedOut = false;
    child.stdout.setEncoding('utf8').on('data', value => { stdout += value; });
    child.stderr.setEncoding('utf8').on('data', value => { stderr += value; });
    const deadline = setTimeout(() => {
      timedOut = true;
      child.kill();
    }, 15000);
    child.once('error', error => {
      clearTimeout(deadline);
      reject(error);
    });
    child.once('close', code => {
      clearTimeout(deadline);
      try {
        assert.ok(!timedOut, 'Browser process exceeded its 15 second test deadline');
        if (rejectNavigation) {
          assert.equal(code, 1, stderr || stdout || 'Expected the browser to reject this navigation');
          resolve({ exitCode: code, diagnostic: stderr.trim() });
        } else {
          assert.equal(code, 0, stderr || stdout);
          const result = JSON.parse(stdout.trim());
          resolve({ title: result.title });
        }
      } catch (error) {
        reject(error);
      }
    });
  });
}

const tests = [];
async function check(name, serve, verify, { rejectNavigation = false } = {}) {
  const trace = [];
  const timers = [];
  const delay = (ms, action) => { timers.push(setTimeout(action, ms)); };
  const server = createServer((request, response) => {
    const path = new URL(request.url, fixture.origin).pathname;
    serve(path, response, fixture, request);
  });
  const fixture = { origin: '', crossOrigin: '', trace, delay };
  try {
    server.listen(0, '127.0.0.1');
    await once(server, 'listening');
    const { port } = server.address();
    fixture.origin = `http://127.0.0.1:${port}`;
    fixture.crossOrigin = `http://localhost:${port}`;
    const result = await runBrowser(fixture.origin + '/', rejectNavigation);
    verify(result, trace);
    tests.push({ name, passed: true, trace, ...result });
  } catch (error) {
    tests.push({ name, passed: false, trace, error: String(error) });
  } finally {
    for (const timer of timers) clearTimeout(timer);
    if (server.listening) {
      const closed = once(server, 'close');
      server.close();
      server.closeAllConnections();
      await closed;
    }
  }
}

let metaDocument;
const metaPrefix = Buffer.concat([
  Buffer.from('<!doctype html><meta charset="windows-1250"><title>Waiting</title><p id="decoded">'),
  Buffer.from([0xbf]),
  Buffer.from("</p><script>var decoded = document.getElementById('decoded').textContent;"
    + "fetch('/meta-gate?decoded=' + (decoded === '\\u017c') + '&state=' + document.readyState);</script>"),
]);
await check('Short meta charset prefix executes before the HTTP document ends', (path, response, { trace, delay }, request) => {
  if (path === '/') {
    assert.ok(metaPrefix.length < 1024, 'The fixture must fit below the encoding scan limit');
    metaDocument = response;
    response.socket.setNoDelay(true);
    response.writeHead(200, { 'Content-Type': 'text/html', 'Cache-Control': 'no-store' });
    response.flushHeaders();
    trace.push('prefix');
    response.write(metaPrefix);
    delay(2000, () => {
      if (response.writableEnded || response.destroyed) return;
      trace.push('fallback-eof');
      response.end('</body>');
    });
  } else if (path === '/meta-gate') {
    trace.push(request.method + ' ' + request.url);
    if (!metaDocument.writableEnded && !metaDocument.destroyed) {
      trace.push('gate-before-eof');
      metaDocument.end("<script>document.title = 'Early meta verified';</script>");
    }
    reply(response, '');
  } else {
    reply(response, 'Unexpected meta prefix route', 'text/plain', 404);
  }
}, (result, trace) => {
  assert.deepEqual(trace, ['prefix', 'GET /meta-gate?decoded=true&state=loading', 'gate-before-eof']);
  assert.equal(result.title, 'Early meta verified');
});

async function checkImage({ name, fails = false, seedScript = false, crossOriginBlocked = false }) {
  let document;
  const event = fails ? 'error' : 'load';
  const secondEvent = crossOriginBlocked ? 'error' : event;
  await check(name, (path, response, { origin, crossOrigin, trace, delay }, request) => {
    const href = crossOriginBlocked ? crossOrigin + '/image' : '/image';
    switch (path) {
      case '/':
        document = response;
        response.socket.setNoDelay(true);
        response.writeHead(200, { 'Content-Type': html, 'Cache-Control': 'no-store' });
        response.flushHeaders();
        trace.push('prefix');
        response.write(`<!doctype html><title>Waiting</title><body>
          <img src="${href}" on${event}="fetch('/first')">
          ${seedScript ? '<script>0;</script>' : ''}`);
        delay(2000, () => {
          if (response.writableEnded || response.destroyed) return;
          trace.push('fallback-eof');
          response.end('</body>');
        });
        break;
      case '/image':
        trace.push(request.headers.origin === origin ? 'image-cors-request' : 'image-request');
        if (fails) reply(response, 'Missing image', 'text/plain', 404);
        else reply(response, png, 'image/png');
        break;
      case '/first':
        trace.push('first-' + event);
        if (!document.writableEnded && !document.destroyed) {
          trace.push('tail');
          document.end(`<img src="${href}" ${crossOriginBlocked ? 'crossorigin="anonymous"' : ''}
            onload="document.title = 'Second load'; fetch('/second-load')"
            onerror="document.title = 'Second error'; fetch('/second-error')"></body>`);
        }
        reply(response, '');
        break;
      case '/second-load':
      case '/second-error':
        trace.push(path.slice(1));
        reply(response, '');
        break;
      default:
        reply(response, 'Unexpected image request', 'text/plain', 404);
    }
  }, (result, trace) => {
    assert.ok(!trace.includes('fallback-eof'), 'The first image did not fire its event before EOF');
    assert.deepEqual(trace.filter(entry => !['image-request', 'image-cors-request'].includes(entry)), [
      'prefix', 'first-' + event, 'tail', 'second-' + secondEvent,
    ]);
    if (!fails) assert.equal(trace.filter(entry => entry === 'image-request').length, 1);
    if (crossOriginBlocked) assert.equal(trace.filter(entry => entry === 'image-cors-request').length, 1);
    assert.equal(result.title, 'Second ' + secondEvent);
  });
}

await checkImage({ name: 'Image discovered before the first input pause' });
await checkImage({ name: 'Later element receives its own load for a cached image', seedScript: true });
await checkImage({ name: 'Later element receives its own error for a missing image', fails: true, seedScript: true });
await checkImage({
  name: 'An opaque image cannot satisfy an anonymous CORS request',
  seedScript: true,
  crossOriginBlocked: true,
});

for (const medium of ['print', 'screen']) {
  let sheet;
  let scriptSeen = false;
  function sendSheet(trace) {
    if (sheet && !sheet.writableEnded && !sheet.destroyed) {
      trace.push('sheet-released');
      reply(sheet, '#target { color: rgb(44, 55, 66) }', css);
    }
  }
  await check(`Stylesheet with media=${medium}`, (path, response, { trace, delay }) => {
    switch (path) {
      case '/':
        reply(response, `<!doctype html><title>Waiting</title>
          <style>#target { color: rgb(11, 22, 33) }</style>
          <link rel="stylesheet" href="/sheet" media="${medium}">
          <p id="target">Style probe</p>
          <script>
            function color() { return getComputedStyle(document.getElementById('target')).color; }
            var firstColor = color();
            fetch('/script');
            window.addEventListener('load', function () { document.title = firstColor + ' / ' + color(); });
          </script>`, html);
        break;
      case '/sheet':
        sheet = response;
        trace.push('sheet-request');
        if (scriptSeen) sendSheet(trace);
        delay(medium === 'screen' ? 500 : 2000, () => sendSheet(trace));
        break;
      case '/script':
        scriptSeen = true;
        trace.push('script');
        sendSheet(trace);
        reply(response, '');
        break;
      default:
        reply(response, 'Unexpected stylesheet request', 'text/plain', 404);
    }
  }, (result, trace) => {
    const color = medium === 'print' ? 'rgb(11, 22, 33)' : 'rgb(44, 55, 66)';
    assert.equal(result.title, `${color} / ${color}`);
    assert.ok(trace.includes('sheet-request') && trace.includes('sheet-released') && trace.includes('script'));
    assert.equal(trace.indexOf('script') < trace.indexOf('sheet-released'), medium === 'print');
  });
}

const hash = (algorithm, body) => `${algorithm}-${createHash(algorithm).update(body).digest('base64')}`;
const acceptedStyle = '#target { color: rgb(77, 88, 99) }';
const rejectedStyle = '#target { color: rgb(33, 22, 11) }';
const opaqueScript = "document.getElementById('target').setAttribute('data-opaque', 'executed');";
const readableScript = "document.getElementById('target').setAttribute('data-readable', 'executed');";
const handlers = `onload="this.setAttribute('data-result', 'load')" onerror="this.setAttribute('data-result', 'error')"`;
await check('Stylesheet SRI and cross-origin script SRI', (path, response, { crossOrigin, trace }) => {
  trace.push(path);
  switch (path) {
    case '/':
      reply(response, `<!doctype html><title>Waiting</title>
        <link id="good" rel="stylesheet" href="/good.css"
          integrity="${hash('sha384', 'different body')} ${hash('sha384', acceptedStyle)}" ${handlers}>
        <link id="bad" rel="stylesheet" href="/bad.css"
          integrity="${hash('sha256', rejectedStyle)} ${hash('sha512', 'different body')}" ${handlers}>
        <p id="target">Integrity probe</p>
        <script id="opaque" src="${crossOrigin}/opaque.js" integrity="${hash('sha256', opaqueScript)}" ${handlers}></script>
        <script id="readable" src="${crossOrigin}/readable.js" crossorigin="anonymous"
          integrity="${hash('sha256', readableScript)}" ${handlers}></script>
        <script>
          window.addEventListener('load', function () {
            var target = document.getElementById('target');
            function result(id) { return document.getElementById(id).getAttribute('data-result'); }
            document.title = JSON.stringify([result('good'), result('bad'), result('opaque'), result('readable'),
              getComputedStyle(target).color, target.getAttribute('data-opaque'), target.getAttribute('data-readable')]);
          });
        </script>`, html);
      break;
    case '/good.css': reply(response, acceptedStyle, css); break;
    case '/bad.css': reply(response, rejectedStyle, css); break;
    case '/opaque.js': reply(response, opaqueScript, js); break;
    case '/readable.js':
      reply(response, readableScript, js, 200, { 'Access-Control-Allow-Origin': '*' });
      break;
    default: reply(response, 'Unexpected integrity request', 'text/plain', 404);
  }
}, result => {
  assert.deepEqual(JSON.parse(result.title), ['load', 'error', 'error', 'load', 'rgb(77, 88, 99)', null, 'executed']);
});

for (const filename of ['untrusted.html', 'café.html']) {
  await check(`HTML attachment with filename="${filename}" stays inert`, (path, response, { trace }) => {
    trace.push(path);
    if (path === '/') {
      reply(response, "<!doctype html><script>document.title = 'Ran attachment'; fetch('/attachment-executed');</script>",
        html, 200, { 'Content-Disposition': `AtTaChMeNt; filename="${filename}"` });
    } else {
      reply(response, '');
    }
  }, (_result, trace) => {
    assert.deepEqual(trace, ['/']);
  }, { rejectNavigation: true });
}

console.log(JSON.stringify({ binary, binarySha256, tests }, null, 2));
if (tests.some(test => !test.passed)) process.exitCode = 1;
