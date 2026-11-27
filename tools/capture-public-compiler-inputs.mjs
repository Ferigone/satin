// Select bounded, public JavaScript inputs from an anonymous Chromium session.
// The resulting private QA directory is for compile-only replay, not publication.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { createRequire } from 'node:module';
import { mkdir, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';

const [executablePath, rea, directory] = process.argv.slice(2);
assert.ok(executablePath && rea && directory && process.argv.length === 5,
  'usage: node tools/capture-public-compiler-inputs.mjs chrome.exe rea.mjs new-directory');
const target = resolve(directory);
await mkdir(target);
const { chromium } = createRequire(resolve(rea))('playwright-core');
const cap = 12 * 1024 * 1024;

function category(site, script) {
  if (script.isModule || script.scriptLanguage !== 'JavaScript' || script.hasSourceURL
    || script.executionContextAuxData?.isDefault !== true
    || script.length < 16384 || script.length > cap) return null;
  let url;
  try { url = new URL(script.url); } catch { return null; }
  if (url.protocol !== 'https:') return null;
  if (site === 'twitch') {
    return url.hostname === 'assets.twitch.tv' && /^\/assets\/core-[a-z0-9.-]+\.js$/i.test(url.pathname)
      ? 'twitch-core' : null;
  }
  // Inline bootstrap text can include anonymous session identifiers. Do not
  // retain it; compare public static assets and separately authored fixtures.
  if (url.hostname !== 'www.youtube.com' || script.startLine !== 0 || script.startColumn !== 0
    || !url.pathname.startsWith('/s/') || !url.pathname.endsWith('.js')) return null;
  return url.pathname.startsWith('/s/player/') ? 'youtube-player' : 'youtube-asset';
}

const cases = [];
const outcome = [];
for (const [site, url] of [
  ['twitch', 'https://www.twitch.tv/theburntpeanut_247'],
  ['youtube', 'https://www.youtube.com/watch?v=aqz-KE-bpKQ'],
]) {
  const browser = await chromium.launch({ executablePath: resolve(executablePath), headless: true });
  try {
    const context = await browser.newContext({ viewport: { width: 1280, height: 800 },
      locale: 'en-US', timezoneId: 'UTC', serviceWorkers: 'allow' });
    const page = await context.newPage();
    const cdp = await context.newCDPSession(page);
    const candidates = new Map();
    cdp.on('Debugger.scriptParsed', script => {
      const tag = category(site, script);
      if (!tag) return;
      const records = candidates.get(tag) ?? [];
      records.push({ scriptId: script.scriptId, length: script.length, line: script.startLine });
      records.sort((a, b) => b.length - a.length);
      candidates.set(tag, records.slice(0, 3));
    });
    await cdp.send('Debugger.enable');
    const status = await page.goto(url, { waitUntil: 'domcontentloaded', timeout: 22000 })
      .then(response => response?.status() ?? null, () => null);
    if (site === 'youtube') {
      await page.getByRole('button', { name: /reject (all|the use of cookies)/i })
        .first().click({ timeout: 3000 }).catch(() => {});
    }
    await page.waitForFunction(() => [...document.querySelectorAll('video')].some(v => v.readyState >= 2),
      null, { timeout: 10000 }).catch(() => {});
    outcome.push({ site, status, chromeVersion: browser.version(), categories: [...candidates.keys()].sort() });
    for (const [tag, records] of [...candidates].sort(([a], [b]) => a.localeCompare(b))) {
      let saved = 0;
      for (const record of records) {
        if (saved === 2) break;
        const item = await cdp.send('Debugger.getScriptSource', { scriptId: record.scriptId }).catch(() => null);
        if (!item?.scriptSource || item.scriptSource.length !== record.length) continue;
        const data = Buffer.from(item.scriptSource);
        if (data.length > cap || data.toString('utf8') !== item.scriptSource) continue;
        const sha256 = createHash('sha256').update(data).digest('hex');
        if (cases.some(item => item.sha256 === sha256)) continue;
        const name = `${tag}-${++saved}`;
        const file = `${name}.js`;
        await writeFile(join(target, file), data, { flag: 'wx' });
        cases.push({ name, file, sha256, bytes: data.length, codeUnits: record.length,
          startLine: record.line, expected: 'classification', evaluate: false });
      }
    }
  } finally {
    await browser.close();
  }
}
await writeFile(join(target, 'manifest.json'), JSON.stringify({ version: 1, cases, outcome }, null, 2) + '\n', { flag: 'wx' });
assert.ok(cases.length > 0, 'No matching public script was captured; no replay can be inferred');
console.log(JSON.stringify({ cases, outcome }, null, 2));
