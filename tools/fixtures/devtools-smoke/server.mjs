import { once } from 'node:events';
import { readFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

const host = '127.0.0.1';
const bodyLimit = 256;
const html = 'text/html; charset=utf-8';
const text = 'text/plain; charset=utf-8';
const page = await readFile(new URL('./page.html', import.meta.url), 'utf8');

function quietPage(title) {
  return `<!doctype html><meta charset="utf-8"><title>${title}</title>
<style>body { margin: 20px; font: 16px sans-serif; background: #eef4f7; }
main { padding: 16px; border: 2px solid #bacad4; background: white; }</style>
<main><h1>${title}</h1><p>No script, subresources, or new Console entries.</p>
<p>Use F12 to inspect this document's Network rows and empty Console.</p></main>`;
}

export async function startFixture() {
  const routes = new Map();
  function route(path, method, status, type, body, headers = {}) {
    routes.set(path, { method, status, type, body, headers,
      count: { served: 0, query: 0, authorization: 0, header: 0, body: 0 } });
  }
  for (const tab of ['a', 'b']) {
    const path = `/devtools-${tab}`;
    route(path, 'GET', 200, html,
      page.replaceAll('__TAB__', tab.toUpperCase()).replaceAll('__LOWER__', tab));
    route(`${path}/fetch-ok`, 'POST', 200, text, 'DT-RESPONSE-BODY', {
      'X-Fixture-Private': 'DT-RESPONSE-HEADER',
      'Set-Cookie': 'fixture=DT-RESPONSE-COOKIE; Path=/; HttpOnly; SameSite=Lax',
    });
    route(`${path}/fetch-missing`, 'GET', 404, text, 'not found');
    route(`${path}/xhr-ok`, 'GET', 200, text, 'xhr ok');
    route(`${path}/xhr-missing`, 'GET', 404, text, 'not found');
  }
  route('/devtools-clean', 'GET', 200, html, quietPage('DevTools clean'));
  route('/devtools-missing', 'GET', 404, html, quietPage('DevTools document 404'));
  route('/devtools-redaction', 'GET', 200, html, quietPage('DevTools redaction'));
  const navAuth = `Basic ${Buffer.from('DT-NAV-USER:DT-NAV-PASS').toString('base64')}`;
  const increment = (value) => Math.min(value + 1, Number.MAX_SAFE_INTEGER);

  function send(response, status, type, body, headers = {}) {
    response.writeHead(status, {
      'Content-Type': type, 'Cache-Control': 'no-store', Connection: 'close',
      'Content-Length': Buffer.byteLength(body), ...headers,
    });
    response.end(body);
  }

  const server = createServer({
    maxHeaderSize: 8192, headersTimeout: 5000, requestTimeout: 5000,
  }, async (request, response) => {
    const [path] = request.url.split('?', 1);
    const query = request.url.slice(path.length + 1);
    const entry = routes.get(path);
    if (!entry || entry.method !== request.method) return send(response, 404, text, 'unknown route');
    let bytes = 0;
    const chunks = [];
    if (Number(request.headers['content-length']) > bodyLimit) {
      return send(response, 413, text, 'fixture request too large');
    }
    try {
      for await (const chunk of request) {
        bytes += chunk.length;
        if (bytes > bodyLimit) return request.destroy();
        chunks.push(chunk);
      }
    } catch {
      // A closed/cancelled request is not a served fixture response.
      return;
    }
    const { count } = entry;
    count.served = increment(count.served);
    if ((path.endsWith('/fetch-ok') && query === 'token=DT-FETCH-QUERY')
        || (path === '/devtools-redaction' && query === 'token=DT-NAV-QUERY')) {
      count.query = increment(count.query);
    }
    if (request.headers.authorization === (path === '/devtools-redaction' ? navAuth : 'Bearer DT-REQUEST-AUTH')) {
      count.authorization = increment(count.authorization);
    }
    if (request.headers['x-fixture-private'] === 'DT-REQUEST-HEADER') count.header = increment(count.header);
    if (Buffer.concat(chunks, bytes).toString() === 'DT-REQUEST-BODY') count.body = increment(count.body);
    send(response, entry.status, entry.type, entry.body, entry.headers);
  });
  server.maxConnections = 16;
  server.maxRequestsPerSocket = 1;
  server.setTimeout(5000, (socket) => socket.destroy());
  server.listen(0, host);
  await once(server, 'listening');

  const port = server.address().port;
  return {
    origin: `http://${host}:${port}`,
    redactionUrl: `http://DT-NAV-USER:DT-NAV-PASS@${host}:${port}/devtools-redaction?token=DT-NAV-QUERY#DT-NAV-FRAGMENT`,
    snapshot: () => Object.fromEntries([...routes].map(([path, { method, status, count }]) =>
      [path, { method, status, ...count }])),
    close: () => new Promise((done, reject) => {
      server.close((error) => error ? reject(error) : done());
      server.closeAllConnections();
    }),
  };
}

if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) {
  const fixture = await startFixture();
  console.log(`A:          ${fixture.origin}/devtools-a`);
  console.log(`B:          ${fixture.origin}/devtools-b`);
  console.log(`Clean:      ${fixture.origin}/devtools-clean`);
  console.log(`404:        ${fixture.origin}/devtools-missing`);
  console.log(`Redaction:  ${fixture.redactionUrl}`);
  console.log('Ctrl+C stops the loopback server and prints response counters, not captured traffic.');
  process.once('SIGINT', async () => {
    await fixture.close();
    console.log(JSON.stringify(fixture.snapshot(), null, 2));
  });
}
