import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdir, mkdtemp, rm, truncate, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { after, test } from 'node:test';
import { fileURLToPath } from 'node:url';

const cli = fileURLToPath(new URL('./inventory.mjs', import.meta.url));
const target = fileURLToPath(new URL('../../target/', import.meta.url));
await mkdir(target, { recursive: true });
const directory = await mkdtemp(join(target, 'wasm-inventory-test-'));
after(() => rm(directory, { recursive: true }));

const header = Buffer.from('0061736d01000000', 'hex');
// A complete 14-byte module with one defined memory, initial 512 pages and no maximum.
const minimalMemory = Buffer.from('0061736d01000000050401008004', 'hex');

function section(id, ...payload) {
  assert.ok(payload.length < 16384);
  const size = payload.length < 128 ? [payload.length] : [(payload.length & 0x7f) | 0x80, payload.length >> 7];
  return Buffer.from([id, ...size, ...payload]);
}

function name(text) {
  const ascii = Buffer.from(text, 'ascii');
  return [ascii.length, ...ascii];
}

async function fixture(name, bytes) {
  const path = join(directory, name);
  await writeFile(path, bytes);
  return path;
}

function run(path) {
  const result = spawnSync(process.execPath, [cli, path], { encoding: 'utf8', timeout: 5000 });
  assert.ifError(result.error);
  return result;
}

function accepted(result) {
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.stderr, '');
  return JSON.parse(result.stdout);
}

function rejected(result, diagnostic) {
  assert.equal(result.status, 1, result.stderr);
  assert.equal(result.stdout, '');
  assert.match(result.stderr, diagnostic);
}

test('reports a declared 512-page initial memory as exactly 32 MiB', async () => {
  const report = accepted(run(await fixture('memory-512.wasm', minimalMemory)));
  assert.equal(report.format, 'wasm-core-v1');
  assert.equal(report.sourceBytes, 14);
  assert.deepEqual(report.sections, {
    counts: { total: 1, standard: 1, custom: 0 },
    order: [{ name: 'memory', payloadBytes: 4, entries: 1 }],
  });
  assert.deepEqual(report.memories, [{
    source: 'definition', addressType: 'i32', shared: false, pageBytes: 65536,
    initialPages: 512, maximumPages: null, initialBytes: 33554432, maximumBytes: null,
  }]);
  assert.deepEqual(report.functions, { imported: 0, defined: 0, codeBodies: 0, types: [] });
  assert.deepEqual(report.satinCheckpoint.observedGaps, ['memory-section']);
});

test('reports import and definition shapes without emitting symbol names or custom payloads', async () => {
  const recordImport = (field, ...descriptor) => [...name('authored-import'), ...name(field), ...descriptor];
  const bytes = Buffer.concat([
    header, section(0, ...name('authored-custom'), 0xff),
    section(1, 3, 0x60, 1, 0x7f, 1, 0x7f, 0x60, 0, 2, 0x7e, 0x7c, 0x60, 1, 0x7f, 0),
    section(2, 5,
      ...recordImport('callback', 0, 0),
      ...recordImport('slots', 1, 0x70, 1, 2, 3),
      ...recordImport('pages', 2, 3, 1, 2),
      ...recordImport('setting', 3, 0x7d, 1),
      ...recordImport('event', 4, 0, 2)),
    section(3, 1, 1), section(4, 1, 0x6f, 0, 1), section(5, 1, 0, 0),
    section(13, 1, 0, 2), section(6, 1, 0x7f, 0, 0x41, 0, 0x0b),
    section(7, 2, ...name('authored-call'), 0, 1, ...name('authored-memory'), 2, 1),
    section(9, 0), section(12, 0),
    section(10, 1, 13, 0, 0x42, 0, 0x44, ...Array(8).fill(0), 0x0b),
    section(11, 0), section(0, ...name('authored-end')),
  ]);
  const result = run(await fixture('shapes.wasm', bytes));
  const report = accepted(result);
  assert.doesNotMatch(result.stdout, /authored/);
  assert.deepEqual(report.sections.counts, { total: 14, standard: 12, custom: 2 });
  assert.deepEqual(report.sections.order.map(section => section.name), [
    'custom', 'type', 'import', 'function', 'table', 'memory', 'tag', 'global', 'export',
    'element', 'data-count', 'code', 'data', 'custom',
  ]);
  assert.deepEqual(report.imports, {
    count: 5, byKind: { function: 1, table: 1, memory: 1, global: 1, tag: 1 },
    globals: [{ valueType: 'f32', mutable: true }], tags: [{ typeIndex: 2 }],
  });
  assert.deepEqual(report.functions, {
    imported: 1, defined: 1, codeBodies: 1,
    types: [
      { parameters: ['i32'], results: ['i32'], imported: 1, defined: 0 },
      { parameters: [], results: ['i64', 'f64'], imported: 0, defined: 1 },
      { parameters: ['i32'], results: [], imported: 0, defined: 0 },
    ],
  });
  assert.deepEqual(report.tables, [
    { source: 'import', elementType: 'funcref', initialElements: 2, maximumElements: 3 },
    { source: 'definition', elementType: 'externref', initialElements: 1, maximumElements: null },
  ]);
  assert.deepEqual(report.memories.map(({ source, shared, initialBytes, maximumBytes }) =>
    ({ source, shared, initialBytes, maximumBytes })), [
    { source: 'import', shared: true, initialBytes: 65536, maximumBytes: 131072 },
    { source: 'definition', shared: false, initialBytes: 0, maximumBytes: null },
  ]);
  assert.equal(report.exports.byKind.memory, 1);
  for (const feature of [
    'non-i32-signature', 'multi-value-result', 'non-function-import', 'non-function-export',
    'shared-memory', 'multiple-memories', 'multiple-tables',
  ]) {
    assert.ok(report.satinCheckpoint.observedGaps.includes(feature), feature);
  }
});

test('does not infer Satin compatibility from an empty list of structural gaps', async () => {
  const bytes = Buffer.concat([
    header, section(1, 1, 0x60, 0, 0), section(3, 1, 0),
    section(10, 1, 5, 0, 0x02, 0x40, 0x0b, 0x0b), // A block, outside the checkpoint's subset.
  ]);
  const path = await fixture('control-flow.wasm', bytes);
  const { satinCheckpoint } = accepted(run(path));
  assert.deepEqual(satinCheckpoint.observedGaps, []);
  assert.match(satinCheckpoint.notInspected.join(' '), /instructions/);

  bytes[bytes.length - 1] = 0x01; // Keep section sizes intact but remove the function's end.
  await writeFile(path, bytes);
  rejected(run(path), /validation: Node rejected/);
});

test('fails closed on truncated sections, corrupt integers and unsupported formats', async () => {
  const inputs = [
    ['truncated', minimalMemory.subarray(0, -1), /malformed: truncated/],
    ['overflow', Buffer.concat([header, Buffer.from('058080808010', 'hex')]), /invalid u32 LEB128/],
    ['non-wasm', Buffer.from('not wasm'), /unsupported:.*wrong magic/],
    ['version', Buffer.from('0061736d02000000', 'hex'), /unsupported:.*version 1/],
    ['unknown-section', Buffer.concat([header, section(14, 0)]), /unsupported: unknown section ID/],
    ['memory64', Buffer.concat([header, section(5, 1, 4, 0)]), /unsupported: memory limits/],
  ];
  for (const [label, bytes, expected] of inputs) {
    rejected(run(await fixture(`${label}.wasm`, bytes)), expected);
  }
});

test('rejects URLs, non-files and inputs over the file-read budget', async () => {
  rejected(run('https://invalid.example/module.wasm'), /input: URLs/);
  rejected(run(directory), /input: expected a regular file/);
  const path = await fixture('oversized.wasm', header);
  await truncate(path, 16 * 1024 * 1024 + 1);
  rejected(run(path), /limit: file exceeds 16777216 bytes/);
});
