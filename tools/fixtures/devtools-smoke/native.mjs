import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { createHash } from 'node:crypto';
import { createReadStream } from 'node:fs';
import { mkdir, mkdtemp, open, realpath, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { startFixture } from './server.mjs';

const [binary, expectedHash, x, y] = process.argv.slice(2);
assert.ok(process.platform === 'win32', 'The native verifier requires Windows.');
assert.ok(binary && /^[a-f0-9]{64}$/i.test(expectedHash ?? '') && [4, 6].includes(process.argv.length),
  'Usage: node native.mjs <verified-dev-exe> <verified-sha256> [network-x network-y]');
const network = x === undefined ? null : { action: 'click_at', x: Number(x), y: Number(y) };
if (network) assert.ok(x.trim() && y.trim() && Number.isFinite(network.x) && Number.isFinite(network.y),
  'Specify logical window coordinates for the candidate’s Network tab.');
const executable = await realpath(binary);
const hash = createHash('sha256');
for await (const chunk of createReadStream(executable)) hash.update(chunk);
const sha256 = hash.digest('hex');
assert.equal(sha256, expectedHash.toLowerCase(), 'Candidate digest does not match.');

const root = fileURLToPath(new URL('../../../target/verification-devtools/', import.meta.url));
await mkdir(root, { recursive: true });
const output = await mkdtemp(join(root, 'native-'));
console.log(`Native captures: ${output}`);
const run = promisify(execFile);
const env = { ...process.env };
for (const key of Object.keys(env)) {
  if (/^(all|https?|no)_proxy$/i.test(key)) delete env[key];
}
env.NO_PROXY = '127.0.0.1';
env.LOCALAPPDATA = join(output, 'local-app-data');
await mkdir(env.LOCALAPPDATA);

const key = (value, control = false) => ({ action: 'key', value, control });
const waitTitle = (value) => ({ action: 'wait_title', value, timeout_ms: 8000 });
const a = '/devtools-a';
const b = '/devtools-b';
const clean = '/devtools-clean';
const missing = '/devtools-missing';
const redact = '/devtools-redaction';
const title = {
  [a]: 'DevTools A ready', [b]: 'DevTools B ready', [clean]: 'DevTools clean',
  [missing]: 'DevTools document 404', [redact]: 'DevTools redaction',
};
const f12 = key('F12');
const plans = [
  ['console-open', a, a, [a], () => [f12]],
  ['console-closed', a, a, [a], () => [f12, f12]],
  ['console-tab-b', a, b, [a, b], (origin) => [f12, ...openB(origin)]],
  ['console-return-a', a, a, [a, b], (origin) => [f12, ...openB(origin), key('Tab', true)]],
  ['console-clean', a, clean, [a, clean], () => [f12, { action: 'click', id: 'clean' }]],
];
if (network) plans.push(
  ['network-a', a, a, [a], () => [f12, network]],
  ['network-b', a, b, [a, b], (origin) => [f12, ...openB(origin), network]],
  ['network-return-a', a, a, [a, b], (origin) => [f12, ...openB(origin), key('Tab', true), network]],
  ['network-clean', a, clean, [a, clean], () => [f12, { action: 'click', id: 'clean' }, waitTitle(title[clean]), network]],
  ['network-404', missing, missing, [missing], () => [f12, network]],
  ['network-redaction', redact, redact, [redact], () => [f12, network]],
);

function openB(origin) {
  return [
    key('t', true), key('l', true), { action: 'text', value: origin + b }, key('Enter'), waitTitle(title[b]),
  ];
}

function checkFixture(observed, documents) {
  const xhr = {};
  for (const path of [a, b, clean, missing, redact]) {
    const visited = documents.includes(path);
    assert.equal(observed[path].served, Number(visited), `Unexpected request count for ${path}`);
    if (path === redact && visited) assert.equal(observed[path].query, 1);
    if (!visited || ![a, b].includes(path)) continue;
    const post = observed[`${path}/fetch-ok`];
    for (const field of ['served', 'query', 'authorization', 'header', 'body']) {
      assert.equal(post[field], 1, `Expected the ${field} canary on ${path}/fetch-ok`);
    }
    assert.equal(observed[`${path}/fetch-missing`].served, 1);
    const xhrOk = observed[`${path}/xhr-ok`].served;
    assert.ok(xhrOk === 0 || xhrOk === 1);
    assert.equal(observed[`${path}/xhr-missing`].served, xhrOk);
    xhr[path] = xhrOk ? 'endpoints observed' : 'not observed';
  }
  return xhr;
}

function checkLegacyDiagnostics(diagnostics, final) {
  assert.ok(Array.isArray(diagnostics), 'Missing legacy native diagnostics array');
  const expected = final === a ? 'A' : final === b ? 'B' : null;
  const text = diagnostics.join('\n');
  for (const tab of ['A', 'B']) {
    if (expected !== tab) assert.ok(!text.includes(`DT-${tab}-`), `Unexpected legacy diagnostics from tab ${tab}`);
  }
  if (!expected) return;
  const prefix = `DT-${expected}-`;
  for (const marker of ['LOG', 'INFO', 'WARN', 'ERROR', 'DEBUG', 'CAUGHT', 'THROWN', 'UNHANDLED']) {
    assert.ok(text.includes(prefix + marker), `Missing legacy diagnostic: ${prefix}${marker}`);
  }
  for (const marker of ['HANDLED-REJECTION', 'HOOK-RAN']) {
    assert.ok(!text.includes(prefix + marker), `Unexpected legacy diagnostic: ${prefix}${marker}`);
  }
}

for (const [name, initial, final, documents, actions] of plans) {
  const fixture = await startFixture();
  try {
    const image = join(output, `${name}.png`);
    const profile = join(output, `${name}-profile`);
    const actionFile = join(output, `${name}-actions.json`);
    await writeFile(actionFile, JSON.stringify([waitTitle(title[initial]), ...actions(fixture.origin),
      waitTitle(title[final])], null, 2));
    const { stdout } = await run(executable, [
      '--url', initial === redact ? fixture.redactionUrl : fixture.origin + initial, '--profile', profile,
      '--smoke-window', image, '--smoke-actions', actionFile, '--width', '1280', '--height', '900',
    ], { cwd: output, windowsHide: true, timeout: 30_000, env });
    const report = JSON.parse(stdout);
    assert.equal(report.window_smoke, true);
    assert.ok(report.frames_presented > 0);
    assert.equal(report.title, title[final]);
    assert.equal(new URL(report.url).pathname, final);
    checkLegacyDiagnostics(report.javascript?.diagnostics, final);
    const png = await open(image, 'r');
    try {
      const header = Buffer.alloc(8);
      await png.read(header);
      assert.equal(header.toString('hex'), '89504e470d0a1a0a');
    } finally {
      await png.close();
    }
    const served = fixture.snapshot();
    const xhr = checkFixture(served, documents);
    await writeFile(join(output, `${name}-receipt.json`), JSON.stringify({
      sha256, version: report.version, title: report.title, frames: report.frames_presented,
      served, xhr, legacyDiagnosticsChecked: true, panelReviewRequired: true,
    }, null, 2));
    console.log(`${name}: native frame and loopback checks passed; review panel in ${name}.png`);
  } finally {
    await fixture.close();
  }
}
if (!network) console.log('Network images not run: supply the candidate’s Network tab coordinates.');
