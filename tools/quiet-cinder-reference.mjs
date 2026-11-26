// Captures the pinned prototype in an existing Chromium binary. Never use this
// for the Satin side: running the prototype as a web page does not test native UI.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { createServer } from 'node:http';
import { mkdir, mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { clippedCenter, comparisonCase, DESIGN_SHA256, viewports, states } from './quiet-cinder-cases.mjs';
import { decodePng } from './quiet-cinder-compare.mjs';

const locale = 'pl-PL', timezone = 'Europe/Warsaw';
const sha256 = bytes => createHash('sha256').update(bytes).digest('hex');
const waitFor = async (description, check) => {
  const deadline = Date.now() + 8000;
  do {
    const value = await check();
    if (value) return value;
    await delay(50);
  } while (Date.now() < deadline);
  throw new Error(`timed out waiting for ${description}`);
};

async function connect(endpoint) {
  const socket = new WebSocket(endpoint);
  const pending = new Map();
  let nextId = 0;
  socket.addEventListener('message', ({ data }) => {
    const { id, result, error } = JSON.parse(data);
    pending.get(id)?.(error, result);
  });
  socket.addEventListener('close', () => {
    for (const finish of pending.values()) finish({ message: 'connection closed' });
  });
  try {
    await new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error('timed out connecting to Chromium CDP')), 8000);
      socket.addEventListener('open', () => { clearTimeout(timer); resolve(); }, { once: true });
      socket.addEventListener('error', () => {
        clearTimeout(timer);
        reject(new Error('could not connect to Chromium CDP'));
      }, { once: true });
    });
  } catch (error) {
    socket.close();
    throw error;
  }
  return {
    close: () => socket.close(),
    send(method, params = {}, sessionId) {
      return new Promise((resolve, reject) => {
        if (socket.readyState !== WebSocket.OPEN) {
          reject(new Error(`Chromium CDP ${method}: socket not open`));
          return;
        }
        const id = ++nextId;
        const finish = (error, value) => {
          clearTimeout(timer);
          pending.delete(id);
          if (error) reject(new Error(`Chromium CDP ${method}: ${error.message}`));
          else resolve(value);
        };
        const timer = setTimeout(() => finish({ message: 'timed out' }), 8000);
        pending.set(id, finish);
        socket.send(JSON.stringify({ id, method, params, sessionId }));
      });
    },
  };
}

const menu = '#menu-button';
const privacy = '#layer [data-act="privacy"]';
const tab = '#top-tab-2';
const visible = selector => `Boolean(document.querySelector(${JSON.stringify(selector)})?.getClientRects().length)`;

async function capture(cdp, info, page, viewport, state, output) {
  const [width, height] = viewports[viewport];
  const { browserContextId } = await cdp.send('Target.createBrowserContext');
  try {
    const { targetId } = await cdp.send('Target.createTarget', { url: 'about:blank', browserContextId });
    const { sessionId } = await cdp.send('Target.attachToTarget', { targetId, flatten: true });
    const send = (method, params) => cdp.send(method, params, sessionId);
    const js = async expression => {
      const { result, exceptionDetails } = await send('Runtime.evaluate', {
        expression, returnByValue: true, awaitPromise: true,
      });
      if (exceptionDetails) throw new Error(`prototype observation failed: ${exceptionDetails.text}`);
      return result.value;
    };
    let pointer;
    const move = async (x, y) => {
      await send('Input.dispatchMouseEvent', { type: 'mouseMoved', x, y });
      pointer = [x, y];
    };
    const park = () => move(2, height - 2);
    const center = async (selectors, clip = null) => {
      const point = await js(`(() => {
        const clipping = ${JSON.stringify(clip)};
        const viewportRect = {left: 0, top: 0, right: innerWidth, bottom: innerHeight};
        const bounds = clipping ? [viewportRect, document.querySelector(clipping).getBoundingClientRect()] : [viewportRect];
        for (const selector of ${JSON.stringify(selectors)}) {
          const target = document.querySelector(selector);
          if (!target) continue;
          const p = (${clippedCenter.toString()})(target.getBoundingClientRect(), ...bounds);
          if (p && document.elementFromPoint(...p)?.closest(selector) === target) return p;
        }
        return null;
      })()`);
      assert.ok(point, `could not hit ${selectors.join(' or ')}: missing, clipped, or covered`);
      return point;
    };
    const click = async selector => {
      const [x, y] = await center([selector]);
      await move(x, y);
      for (const type of ['mousePressed', 'mouseReleased']) {
        await send('Input.dispatchMouseEvent', { type, x, y, button: 'left', clickCount: 1 });
      }
    };
    const escape = async () => {
      for (const type of ['rawKeyDown', 'keyUp']) {
        await send('Input.dispatchKeyEvent', {
          type, key: 'Escape', code: 'Escape', windowsVirtualKeyCode: 27,
        });
      }
    };

    await send('Emulation.setDeviceMetricsOverride', {
      width, height, deviceScaleFactor: 1, mobile: false,
    });
    await send('Emulation.setEmulatedMedia', { features: [{ name: 'prefers-reduced-motion', value: 'reduce' }] });
    await send('Emulation.setTimezoneOverride', { timezoneId: timezone });
    await send('Emulation.setLocaleOverride', { locale: 'pl_PL' });
    await send('Emulation.setUserAgentOverride', { userAgent: info.userAgent, acceptLanguage: locale });
    const navigation = await send('Page.navigate', { url: page });
    assert.ok(!navigation.errorText, navigation.errorText);
    await waitFor('prototype new tab', () => js(`document.readyState === 'complete' &&
      document.querySelector('.home-wordmark')?.textContent === 'Satin'`));
    await send('Page.bringToFront');
    await park();
    assert.ok(await js(`(() => {
      const b = document.querySelector('#browser').getBoundingClientRect();
      const d = document.body.dataset;
      return b.x === 0 && b.y === 0 && b.width === ${width} && b.height === ${height} &&
        innerWidth === ${width} && innerHeight === ${height} && devicePixelRatio === 1 &&
        visualViewport.scale === 1 && visualViewport.width === ${width} && visualViewport.height === ${height} &&
        document.querySelector('#reload-button').getBoundingClientRect().width === ${width <= 410 ? 25 : 31} &&
        document.documentElement.lang === 'pl' &&
        navigator.language === '${locale}' &&
        Intl.DateTimeFormat().resolvedOptions().locale === '${locale}' &&
        Intl.DateTimeFormat().resolvedOptions().timeZone === '${timezone}' &&
        matchMedia('(prefers-reduced-motion: reduce)').matches &&
        !matchMedia('(pointer: coarse)').matches && !matchMedia('(forced-colors: active)').matches &&
        d.palette === 'amethyst' && d.tabShape === 'capsule' && d.experiment === 'quiet' &&
        document.body.hasAttribute('data-fixed-appearance') &&
        document.querySelectorAll('#tabs [role=tab]').length === 3 &&
        document.querySelector('#top-tab-1').getAttribute('aria-selected') === 'true' &&
        document.querySelectorAll('#favorites .favorite').length === 4;
    })()`), 'unexpected reference environment or initial prototype state');

    let expected = '!document.querySelector("#layer .popup") && !document.querySelector("#browser").classList.contains("is-quiet")';
    if (state === 'tab-hover') {
      // At 390 a sliver of the inactive tab can remain after the label clips.
      // Hover that face, not the neighboring + button, without scrolling.
      const targets = [`${tab} .tab-label`];
      if (viewport === 'narrow-390') targets.push(tab);
      await move(...await center(targets, '#tabs .tab-list'));
      expected += ` && document.querySelector('${tab}').matches(':hover[aria-selected=false]') &&
        document.querySelector('#tabs .tab-list').scrollLeft === 0 &&
        document.querySelector('#tab-peek').hidden && getComputedStyle(document.querySelector('${tab} .tab-x')).opacity === '1'`;
    } else if (state === 'quiet') {
      await click('#quiet-button');
      await park();
      expected = `document.querySelector('#quiet-button').getAttribute('aria-pressed') === 'true' &&
        document.querySelector('#favorites').getBoundingClientRect().height === 0`;
    } else if (state === 'address') {
      await click('#address');
      await send('Input.insertText', { text: 'sec' });
      await park();
      expected = `document.activeElement.id === 'address' && document.activeElement.value === 'sec' &&
        document.querySelector('#suggest-0')?.textContent.includes('Szukaj') &&
        document.querySelector('#suggest-1')?.textContent.includes('Granią Secedy')`;
    } else if (['menu', 'menu-hover', 'menu-dismiss', 'settings', 'bookmarks', 'downloads'].includes(state)) {
      await click(menu);
      await waitFor('browser menu', () => js(visible(privacy)));
      if (state === 'menu-hover') {
        await move(...await center([privacy], '#layer .popup'));
        expected = `document.querySelector('${privacy}').matches(':hover')`;
      } else {
        await park();
        if (state === 'menu') expected = visible(privacy);
        else if (state === 'menu-dismiss') {
          await escape();
          expected += ` && document.activeElement.id === 'menu-button'`;
        } else {
          const row = `#layer [data-act="internal"][data-val="${state}"]`;
          await js(`document.querySelector(${JSON.stringify(row)}).scrollIntoView({block:'nearest'})`);
          await click(row);
          await park();
          const heading = { settings: 'Ustawienia', bookmarks: 'Zakładki', downloads: 'Pobrane pliki' }[state];
          expected += ` && document.querySelector('.internal-head h1')?.textContent === '${heading}'`;
        }
      }
    }
    await js('document.fonts.ready.then(() => true)');
    await js('new Promise(done => requestAnimationFrame(() => requestAnimationFrame(() => done(true))))');
    assert.ok(await js(expected), `did not reach ${state}`);
    const landmarks = await js(`Object.fromEntries([
      '#browser', '#tabs', '#tabs .tab-list', '#top-tab-2', '#top-tab-2 .tab-label', '#top-tab-2 .tab-x',
      '#toolbar', '#reload-button', '#menu-button', '#address-form', '#favorites',
      '.home-wordmark', '.start-search', '#layer .popup',
      '#layer .menu-row[data-act="privacy"]', '.internal-head', '.settings-nav',
      '.setting .toggle', '.data-list .data-row',
    ].map(s => [s, document.querySelector(s)?.getBoundingClientRect().toJSON() ?? null]))`);
    await send('DOM.enable');
    await send('CSS.enable');
    const { root } = await send('DOM.getDocument');
    const fonts = {};
    for (const selector of ['.tab-label', '#address', '.home-wordmark', '.internal-head h1', '#layer .menu-row .row-text']) {
      const { nodeId } = await send('DOM.querySelector', { nodeId: root.nodeId, selector });
      if (nodeId) fonts[selector] = (await send('CSS.getPlatformFontsForNode', { nodeId })).fonts;
    }
    const { data } = await send('Page.captureScreenshot', {
      format: 'png', fromSurface: true, captureBeyondViewport: false,
    });
    const bytes = Buffer.from(data, 'base64');
    const image = decodePng(bytes);
    assert.deepEqual([image.width, image.height], [width, height]);
    const filename = `${viewport}-${state}.png`;
    await writeFile(join(output, filename), bytes, { flag: 'wx' });
    return { viewport, state, filename, size: [width, height], pointer, sha256: sha256(bytes), landmarks, fonts };
  } finally {
    await cdp.send('Target.disposeBrowserContext', { browserContextId });
  }
}

async function main([executable, design, dir, selectedCase, ...extra]) {
  if (!executable || !design || !dir || extra.length) {
    throw new Error('usage: node tools/quiet-cinder-reference.mjs CHROMIUM.exe Satin-Quiet-Cinder.html OUT_DIR [CASE]');
  }
  const cases = Object.keys(viewports).flatMap(viewport => states.map(state => `${viewport}/${state}`));
  if (selectedCase) comparisonCase(selectedCase);
  const bytes = await readFile(design);
  assert.equal(sha256(bytes), DESIGN_SHA256, 'prototype SHA-256 differs from the pinned design');
  await mkdir(dir, { recursive: true });
  const output = await mkdtemp(join(resolve(dir), 'chromium-'));
  const profile = join(output, 'disposable-profile');
  const server = createServer((request, response) => {
    if (request.method !== 'GET' || request.url !== '/Satin-Quiet-Cinder.html') {
      response.writeHead(404).end();
      return;
    }
    response.writeHead(200, {
      'Content-Type': 'text/html; charset=utf-8', 'Content-Length': bytes.length, 'Cache-Control': 'no-store',
    }).end(bytes);
  });
  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', resolve);
  });
  const page = `http://127.0.0.1:${server.address().port}/Satin-Quiet-Cinder.html`;
  const args = [
    '--headless', `--user-data-dir=${profile}`, '--remote-debugging-address=127.0.0.1',
    '--remote-debugging-port=0', '--no-first-run', '--no-default-browser-check',
    '--disable-background-networking', '--disable-component-update',
    '--force-color-profile=srgb', '--lang=pl-PL', 'about:blank',
  ];
  let cdp, startupError, child, childExit;
  try {
    child = spawn(resolve(executable), args, { windowsHide: true, stdio: 'ignore' });
    childExit = new Promise(resolve => child.once('exit', resolve));
    child.once('error', error => { startupError = error; });
    const endpoint = await waitFor('Chromium debugging endpoint', async () => {
      if (startupError) throw startupError;
      if (child.exitCode !== null) throw new Error(`Chromium exited with ${child.exitCode}`);
      const value = await readFile(join(profile, 'DevToolsActivePort'), 'utf8').catch(error => {
        if (error.code !== 'ENOENT') throw error;
        return '';
      });
      const [port, path] = value.trim().split(/\r?\n/);
      if (/^\d+$/.test(port) && Number(port) > 0 && Number(port) < 65536 &&
          /^\/devtools\/browser\/[a-f0-9-]+$/.test(path)) return `ws://127.0.0.1:${port}${path}`;
      return null;
    });
    cdp = await connect(endpoint);
    const info = await cdp.send('Browser.getVersion');
    const captures = [];
    for (const name of selectedCase ? [selectedCase] : cases) {
      captures.push(await capture(cdp, info, page, ...name.split('/'), output));
    }
    await writeFile(join(output, 'reference.json'), `${JSON.stringify({
      design: resolve(design), designSha256: DESIGN_SHA256, executable: resolve(executable),
      chromium: info, locale, timezone, dpr: 1, colorProfile: 'sRGB',
      reducedMotion: true, simulatedData: true, captures,
    }, null, 2)}\n`, { flag: 'wx' });
    console.log(output);
  } catch (error) {
    throw new Error(`${error.message} (partial reference artifacts: ${output})`);
  } finally {
    server.close();
    server.closeAllConnections();
    if (cdp) {
      const closing = cdp.send('Browser.close').catch(() => {});
      await Promise.race([childExit, delay(1500, undefined, { ref: false })]);
      cdp.close();
      await closing;
    }
    if (child?.pid && child.exitCode === null) child.kill();
  }
}

try {
  await main(process.argv.slice(2));
} catch (error) {
  console.error(error.message);
  process.exitCode = 2;
}
