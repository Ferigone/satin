import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { once } from 'node:events';
import { mkdir, mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { resolve } from 'node:path';
import { promisify } from 'node:util';

const [exe = 'target/release/rust-browser.exe'] = process.argv.slice(2);
const binary = resolve(exe);
const run = promisify(execFile);
const directory = resolve('target/bookmark-verification');
await mkdir(directory, { recursive: true });
const output = await mkdtemp(resolve(directory, 'run-'));
const profile = resolve(output, 'profile');
const store = resolve(profile, 'bookmarks.json');

const alpha = { title: 'Authored page Alpha', path: '/alpha.html' };
const beta = { title: 'Authored page Beta', path: '/beta.html?edition=2#intro' };
const pages = new Map([
  [alpha.path, `<h1>${alpha.title}</h1><p>Local fixture for saving and removing the current page.</p>
     <p><a id="to-beta" href="${beta.path}">Open the second authored page</a></p>`],
  ['/beta.html', `<h1 id="intro">${beta.title}</h1><p>Local fixture with a query and fragment.</p>
     <p><a id="to-alpha" href="${alpha.path}">Return to the first authored page</a></p>`],
]);
const server = createServer((request, response) => {
  const pathname = new URL(request.url, 'http://127.0.0.1').pathname;
  const content = pages.get(pathname);
  if (!content) return response.writeHead(404).end();
  const title = pathname === alpha.path ? alpha.title : beta.title;
  response.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8', 'Cache-Control': 'no-store' })
    .end(`<!doctype html><meta charset=utf-8><title>${title}</title>
      <style>body{background:#f1f1f7;margin:2em;color:#2a2542}header,footer{background:#ddd9ed;padding:1em}
      main{background:white;padding:1em}a{color:#2e2964}</style>
      <header><strong>Satin bookmarks verification</strong></header>
      <main>${content}</main><footer><small>Loopback-only test document</small></footer>`);
});
const listening = once(server, 'listening');
server.listen(0, '127.0.0.1');
await listening;
const origin = `http://127.0.0.1:${server.address().port}`;
const key = (value, modifiers = {}) => ({ action: 'key', value, ...modifiers });
const wait = value => ({ action: 'wait_title', value, timeout_ms: 8000 });
const save = key('d', { control: true });
const openBookmarks = [
  key('f', { alt: true }), key('ArrowDown'), key('ArrowDown'), key('ArrowDown'), key('Enter'),
];
const expected = (...entries) => ({
  schema: 1,
  entries: entries.map(({ title, path }) => ({ title, url: origin + path })),
});

async function smoke(name, url, actions, finalPage) {
  const actionsPath = resolve(output, `${name}.json`);
  await writeFile(actionsPath, JSON.stringify(actions, null, 2));
  const { stdout } = await run(binary, [
    '--profile', profile, '--url', url, '--smoke-actions', actionsPath,
    '--smoke-window', resolve(output, `${name}.png`), '--width', '1000', '--height', '720',
  ], { timeout: 30000, windowsHide: true });
  const summary = JSON.parse(stdout);
  assert.equal(summary.title, finalPage.title);
  assert.equal(summary.url, origin + finalPage.path);
  assert.ok(summary.frames_presented > 0);
}

try {
  await smoke('save', origin + alpha.path, [
    wait(alpha.title), save, save, key('Escape'),
    { action: 'click', id: 'to-beta' }, wait(beta.title),
    ...openBookmarks, key('Enter'),
  ], beta);
  assert.deepEqual(JSON.parse(await readFile(store, 'utf8')), expected(beta, alpha));

  await smoke('reopen-and-remove-current', 'about:home', [
    save, key('Escape'), ...openBookmarks, key('ArrowDown'), key('Enter'), wait(alpha.title),
    ...openBookmarks, key('Enter'),
  ], alpha);
  assert.deepEqual(JSON.parse(await readFile(store, 'utf8')), expected(beta));

  await smoke('reopen-and-delete-selected', 'about:home', [
    ...openBookmarks, key('Enter'), wait(beta.title),
    { action: 'click', id: 'to-alpha' }, wait(alpha.title),
    ...openBookmarks, key('ArrowDown'), key('Delete'),
  ], alpha);
  assert.deepEqual(JSON.parse(await readFile(store, 'utf8')), expected());
  console.log(JSON.stringify({ ok: true, nativeRuns: 3, output }));
} finally {
  server.closeAllConnections();
  await new Promise(done => server.close(done));
}
