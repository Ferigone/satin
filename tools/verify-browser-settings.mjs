import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdir, mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';

const [binaryInput, outputInput] = process.argv.slice(2);
assert.ok(binaryInput && outputInput, 'Usage: node tools/verify-browser-settings.mjs BROWSER.exe OUTPUT');
const binary = resolve(binaryInput);
const outputRoot = resolve(outputInput);
await mkdir(outputRoot, { recursive: true });
const output = await mkdtemp(join(outputRoot, 'run-'));
const profile = join(output, 'profile');
await mkdir(profile);
const savedSession = { schema: 1, tabs: ['about:graphics', 'about:version'], active: 1 };
const sessionPath = join(profile, 'session.json');
const settingsPath = join(profile, 'Settings.v1.json');
const readJson = async path => JSON.parse(await readFile(path, 'utf8'));
await writeFile(sessionPath, JSON.stringify(savedSession));

const key = (value, modifiers = {}) => ({ action: 'key', value, ...modifiers });
const click = (x, y) => ({ action: 'click_at', x, y });
const title = value => ({ action: 'title', value });
const text = value => ({ action: 'text', value });
const openSettings = [key('f', { alt: true }), key('ArrowDown'), key('ArrowDown'), key('Enter')];
const address = value => [key('l', { control: true }), text(value), key('Enter')];
async function launch(name, actions = [], url) {
  const actionsPath = join(output, `${name}.actions.json`);
  await writeFile(actionsPath, JSON.stringify(actions));
  const args = ['--profile', profile, '--smoke-window', join(output, `${name}.png`),
    '--smoke-actions', actionsPath];
  if (url) args.push('--url', url);
  const result = spawnSync(binary, args, { encoding: 'utf8', timeout: 45_000, windowsHide: true });
  await writeFile(join(output, `${name}.stdout.json`), result.stdout ?? '');
  if (result.stderr) await writeFile(join(output, `${name}.stderr.log`), result.stderr);
  if (result.error) throw result.error;
  assert.equal(result.status, 0, `${name}: ${result.stderr ?? ''}`);
  const receipt = JSON.parse(result.stdout);
  assert.equal(receipt.window_smoke, true, name);
  assert.ok(receipt.frames_presented > 0, name);
  return receipt;
}

const defaults = await launch('01-default', openSettings);
assert.equal(defaults.url, 'about:version');
assert.deepEqual(await readJson(sessionPath), savedSession);
await assert.rejects(readJson(settingsPath), { code: 'ENOENT' });

const toggle = await launch('02-keyboard-toggle', [
  ...openSettings, key('Tab'), key('Tab', { shift: true }), key('Space'),
  // Browser shortcuts, text, and clicks do not reach the underlying tab and toolbar.
  key('t', { control: true }), key('l', { control: true }), text('about:home'),
  click(1042, 90), title('About Satin'), key('Tab'), key('Enter'),
  // Done restored browser key handling; Escape does the same on the next visit.
  ...openSettings, key('Escape'), ...address('about:graphics'), ...openSettings,
]);
assert.equal(toggle.url, 'about:graphics');
assert.deepEqual(await readJson(settingsPath), { startup: 'start_page' });
assert.deepEqual(await readJson(sessionPath), { ...savedSession, tabs: ['about:graphics', 'about:graphics'] });

const startPage = await launch('03-restart-start-page');
assert.equal(startPage.url, 'about:home');
assert.deepEqual(await readJson(sessionPath), { schema: 1, tabs: ['about:home'], active: 0 });
assert.equal((await launch('04-explicit-url', [], 'about:version')).url, 'about:version');

// Default smoke geometry is 1180x840 logical pixels. Open Settings by mouse
// from the toolbar, click the checkbox row, then dismiss with the Done button.
const restore = await launch('05-mouse-toggle', [
  title('New Tab'), click(1150, 90), click(1060, 220), click(580, 401), click(763, 500),
  key('t', { control: true }), ...address('about:graphics'),
]);
assert.equal(restore.url, 'about:graphics');
assert.deepEqual(await readJson(settingsPath), { startup: 'restore_tabs' });
// This is a new launch while Start page is still selected. The preceding
// explicit URL applied only to its own process, and toggling does not replace
// the current start page with a tab from an earlier process.
const latest = { schema: 1, tabs: ['about:home', 'about:graphics'], active: 1 };
assert.deepEqual(await readJson(sessionPath), latest);
assert.equal((await launch('06-restart-restore')).url, 'about:graphics');
assert.deepEqual(await readJson(sessionPath), latest);
console.log(`Browser settings verified: ${output}`);
