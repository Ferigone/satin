import assert from 'node:assert/strict';
import { test } from 'node:test';
import { createContext, runInContext } from 'node:vm';
import { startFixture } from './server.mjs';

test('tab pages emit distinct scenarios and send Fetch canaries to loopback', { timeout: 10_000 }, async (t) => {
  const fixture = await startFixture();
  t.after(fixture.close);

  for (const tab of ['A', 'B']) {
    const path = `/devtools-${tab.toLowerCase()}`;
    const page = await fetch(fixture.origin + path);
    assert.equal(page.status, 200);
    assert.equal(page.headers.get('cache-control'), 'no-store');
    const html = await page.text();
    const nodes = new Map(['fetch-result', 'xhr-result', 'coercion-count', 'burst']
      .map((id) => [id, { textContent: '' }]));
    const document = { title: '', getElementById: (id) => nodes.get(id) };
    const logs = [];
    const thrown = [];
    const rejections = [];
    const context = createContext({
      document,
      fetch: (url, options) => fetch(fixture.origin + url, options),
      console: Object.fromEntries(['log', 'info', 'warn', 'error', 'debug'].map((level) =>
        [level, (value) => logs.push([level, value])])),
      // Collect the authored rejection calls without triggering the Node test
      // runner's unhandled rejection policy. This is not a Promise host test.
      Promise: {
        reject: (value) => {
          const event = { value, handled: false };
          rejections.push(event);
          return { catch: () => { event.handled = true; } };
        },
        resolve: Promise.resolve.bind(Promise),
      },
    });
    const scripts = [...html.matchAll(/<script>([\s\S]*?)<\/script>/g)];
    assert.equal(scripts.length, 5);
    for (const [, source] of scripts) {
      try {
        await runInContext(source, context, { timeout: 1000 });
      } catch (error) {
        thrown.push(error);
      }
    }
    const prefix = `DT-${tab}-`;
    assert.deepEqual(logs, ['log', 'info', 'warn', 'error', 'debug', 'info'].map((level, idx) =>
      [level, prefix + ['LOG', 'INFO', 'WARN', 'ERROR', 'DEBUG', 'CAUGHT'][idx]]));
    assert.equal(thrown.length, 2);
    assert.equal(thrown[0], prefix + 'THROWN');
    assert.equal(thrown[1], context.hostile);
    assert.equal(rejections.length, 3);
    assert.deepEqual(rejections.slice(0, 2), [
      { value: prefix + 'HANDLED-REJECTION', handled: true },
      { value: prefix + 'UNHANDLED', handled: false },
    ]);
    assert.equal(rejections[2].value, context.hostile);
    assert.equal(rejections[2].handled, false);
    assert.equal(context.coercions, 0);
    assert.equal(document.title, `DevTools ${tab} ready`);
    assert.equal(nodes.get('fetch-result').textContent, 'Fetch: 200/404');
    assert.equal(nodes.get('xhr-result').textContent, 'XHR: unsupported');
    nodes.get('burst').onclick();
    assert.equal(logs.length, 526);
    assert.deepEqual(logs.at(-1), ['debug', prefix + 'BURST-519']);

    const observed = fixture.snapshot();
    assert.deepEqual(observed[`${path}/fetch-ok`], {
      method: 'POST', status: 200, served: 1, query: 1, authorization: 1, header: 1, body: 1,
    });
    assert.equal(observed[`${path}/fetch-missing`].served, 1);
    assert.equal(observed[`${path}/xhr-ok`].served, 0);
    assert.equal(observed[`${path}/xhr-missing`].served, 0);
  }
});

test('server bounds and isolates requests; XHR endpoints are real HTTP routes', { timeout: 10_000 }, async (t) => {
  const fixture = await startFixture();
  t.after(fixture.close);
  async function get(path, status) {
    const response = await fetch(fixture.origin + path);
    assert.equal(response.status, status);
    return response.text();
  }
  assert.doesNotMatch(await get('/devtools-clean', 200), /<script\b/i);
  assert.match(await get('/devtools-missing', 404), /DevTools document 404/);
  const redaction = new URL(fixture.redactionUrl);
  assert.equal(redaction.origin, fixture.origin);
  assert.equal(redaction.username, 'DT-NAV-USER');
  assert.equal(redaction.password, 'DT-NAV-PASS');
  assert.equal(redaction.hash, '#DT-NAV-FRAGMENT');
  await get(redaction.pathname + redaction.search + redaction.hash, 200);
  assert.equal(fixture.snapshot()[redaction.pathname].query, 1);
  assert.equal(await get('/devtools-b/xhr-ok', 200), 'xhr ok');
  assert.equal(await get('/devtools-b/xhr-missing', 404), 'not found');
  assert.equal(fixture.snapshot()['/devtools-a/xhr-ok'].served, 0);
  assert.equal(await get('/devtools-b/fetch-ok', 404), 'unknown route');
  assert.equal(await get('/unknown', 404), 'unknown route');
  const oversized = await fetch(fixture.origin + '/devtools-b/fetch-ok', {
    method: 'POST', body: 'x'.repeat(257),
  });
  assert.equal(oversized.status, 413);
  await oversized.text();
  assert.equal(fixture.snapshot()['/devtools-b/fetch-ok'].served, 0);
});
