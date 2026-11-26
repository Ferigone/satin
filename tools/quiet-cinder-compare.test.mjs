import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { crc32, deflateSync } from 'node:zlib';
import { comparisonCase, viewports, states } from './quiet-cinder-cases.mjs';
import { compare, decodePng, measure } from './quiet-cinder-compare.mjs';

function chunk(type, body) {
  const bytes = Buffer.alloc(12 + body.length);
  bytes.writeUInt32BE(body.length);
  bytes.write(type, 4);
  body.copy(bytes, 8);
  bytes.writeUInt32BE(crc32(bytes.subarray(4, -4)), bytes.length - 4);
  return bytes;
}

function png(w, h, rgba, scanlines, chunkSize) {
  const header = Buffer.alloc(13);
  header.writeUInt32BE(w, 0);
  header.writeUInt32BE(h, 4);
  header[8] = 8;
  header[9] = rgba ? 6 : 2;
  const compressed = deflateSync(scanlines);
  const size = chunkSize ?? compressed.length;
  const idat = [];
  for (let i = 0; i < compressed.length; i += size) {
    idat.push(chunk('IDAT', compressed.subarray(i, i + size)));
  }
  return Buffer.concat([
    Buffer.from('89504e470d0a1a0a', 'hex'), chunk('IHDR', header), ...idat, chunk('IEND', Buffer.alloc(0)),
  ]);
}

function image(width, height, value) {
  const rgb = Buffer.alloc(width * height * 3);
  for (let y = 0; y < height; ++y) {
    for (let x = 0; x < width; ++x) {
      const v = value(x, y);
      const i = (y * width + x) * 3;
      rgb.set(typeof v === 'number' ? [v, v, v] : v, i);
    }
  }
  return { width, height, rgb };
}

function writeTestImage({ width, height, rgb }) {
  const scanlines = Buffer.alloc((width * 3 + 1) * height);
  for (let y = 0; y < height; ++y) {
    const i = y * width * 3;
    rgb.copy(scanlines, i + y + 1, i, i + width * 3);
  }
  return png(width, height, false, scanlines);
}

test('PNG supports all five row filters and split IDAT, including byte wraparound', () => {
  const encoded = png(2, 5, false, Buffer.from([
    0, 10, 10, 10, 250, 250, 250,
    1, 20, 20, 20, 241, 241, 241,
    2, 10, 10, 10, 20, 20, 20,
    3, 225, 225, 225, 196, 196, 196,
    4, 45, 45, 45, 55, 55, 55,
  ]), 7);
  const decoded = decodePng(encoded);
  assert.deepEqual([decoded.width, decoded.height], [2, 5]);
  assert.deepEqual([...decoded.rgb], [10, 250, 20, 5, 30, 25, 240, 72, 29, 84].flatMap(v => [v, v, v]));
});

test('opaque RGBA normalizes to RGB; transparency, corrupt CRC, oversized zlib and invalid filter fail', () => {
  const opaque = png(1, 1, true, Buffer.from([0, 12, 34, 56, 255]));
  assert.deepEqual([...decodePng(opaque).rgb], [12, 34, 56]);
  assert.throws(() => decodePng(png(1, 1, true, Buffer.from([0, 12, 34, 56, 0]))), /non-opaque/);
  const corrupt = Buffer.from(opaque);
  corrupt[24] ^= 4;
  assert.throws(() => decodePng(corrupt), /CRC/);
  assert.throws(() => decodePng(opaque.subarray(0, -12)), /incomplete/);
  assert.throws(() => decodePng(png(1, 1, false, Buffer.alloc(6))), /Cannot create|larger than/);
  assert.throws(() => decodePng(png(1, 1, false, Buffer.from([5, 0, 0, 0]))), /invalid PNG filter/);
});

test('threshold is inclusive, MAE counts all channels, and bounds use client origin', () => {
  const a = image(3, 2, () => 10);
  const b = image(3, 2, (x, y) => !y ? 255 : x === 1 ? [21, 10, 10] : [10, 22, 14]);
  const metrics = measure(a, b, [1, 1, 2, 1]);
  assert.deepEqual(metrics, {
    pixels: 2, changedPixels: 1, meanAbsoluteRgb: 27 / 6, changedPercent: 50,
    maxChannelDelta: 12, referenceEdgePercent: 0, changedBounds: [2, 1, 1, 1],
  });
  assert.throws(() => measure(a, image(2, 2, () => 0), [0, 0, 1, 1]), /dimensions differ/);
  assert.throws(() => measure(a, b, [2, 1, 2, 1]), /outside image/);
});

test('each budget is independent, and a flat reference is invalid even if identical', () => {
  const spec = { size: [20, 10], crops: [{ name: 'text', rect: [2, 2, 10, 5], maxMeanRgb: 3, maxChangedPercent: 7 }] };
  const v = x => x % 2 ? 50 : 20;
  const a = image(20, 10, v);
  assert.equal(compare(spec, a, a).status, 'within-budgets');
  assert.equal(compare(spec, a, image(20, 10, x => v(x) + 4)).status, 'outside-budgets');
  const fewChanged = image(20, 10, (x, y) => x >= 2 && x < 6 && y === 2 ? [v(x) + 12, v(x), v(x)] : v(x));
  const { crops: [crop] } = compare(spec, a, fewChanged);
  assert.equal(crop.changedPercent, 8);
  assert.equal(crop.meanAbsoluteRgb, 0.32);
  assert.equal(crop.status, 'outside-budgets');
  const flat = image(20, 10, () => 50);
  assert.equal(compare(spec, flat, flat).status, 'invalid-reference');
  assert.throws(() => compare(spec, flat, image(19, 10, v)), /expected 20×10/);
});

test('all planned viewports and states have bounded, nontrivial crops', () => {
  for (const [viewport, size] of Object.entries(viewports)) {
    for (const state of states) {
      const spec = comparisonCase(`${viewport}/${state}`);
      assert.deepEqual(spec.size, size);
      assert.ok(spec.crops.length);
      for (const { rect: [x, y, w, h] } of spec.crops) {
        assert.ok(w * h >= 1000);
        assert.ok(x >= 0 && y >= 0 && x + w <= size[0] && y + h <= size[1]);
      }
    }
  }
  assert.equal(comparisonCase('desktop/downloads').dataScope, 'design-only-demo-data');
  assert.equal(comparisonCase('desktop/address').dataScope, 'requires-matched-data');
  assert.throws(() => comparisonCase('desktop/idle/extra'), /unknown case/);
});

test('CLI reports crop differences that barely affect the frame and emits a readable heatmap', async () => {
  const dir = await mkdtemp(join(tmpdir(), 'satin-quiet-cinder-test-'));
  try {
    const ref = join(dir, 'ref.png'), native = join(dir, 'native.png'), diff = join(dir, 'diff.png');
    const v = x => x % 2 ? 50 : 20;
    const imageA = writeTestImage(image(640, 480, v));
    await writeFile(ref, imageA);
    const cli = fileURLToPath(new URL('./quiet-cinder-compare.mjs', import.meta.url));
    await writeFile(native, imageA);
    const matched = spawnSync(process.execPath, [cli, 'narrow/tab-hover', ref, native], { encoding: 'utf8' });
    assert.equal(matched.status, 0, matched.stderr);
    assert.equal(JSON.parse(matched.stdout).status, 'within-budgets');
    await writeFile(native, writeTestImage(image(640, 480, (x, y) =>
      x >= 155 && x < 190 && y >= 5 && y < 39 ? v(x) + 50 : v(x))));
    const failed = spawnSync(process.execPath, [cli, 'narrow/tab-hover', ref, native, diff], { encoding: 'utf8' });
    assert.equal(failed.status, 1, failed.stderr);
    const result = JSON.parse(failed.stdout);
    assert.ok(result.wholeFrame.changedPercent < 1);
    assert.equal(result.crops.find(c => c.name === 'inactive-tab').status, 'outside-budgets');
    const heatmap = decodePng(await readFile(diff));
    assert.equal(heatmap.rgb[(5 * 640 + 155) * 3], 200);
    const wrongSize = spawnSync(process.execPath, [cli, 'desktop/tab-hover', ref, native], { encoding: 'utf8' });
    assert.equal(wrongSize.status, 2);
    assert.match(wrongSize.stderr, /expected 1280×800/);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test('reference CLI rejects unpinned input before launching or writing capture artifacts', async () => {
  const dir = await mkdtemp(join(tmpdir(), 'satin-quiet-cinder-pin-'));
  try {
    const wrong = join(dir, 'wrong.html'), artifacts = join(dir, 'not-created');
    await writeFile(wrong, '<!doctype html><title>Wrong design</title>');
    const cli = fileURLToPath(new URL('./quiet-cinder-reference.mjs', import.meta.url));
    const result = spawnSync(process.execPath, [cli, join(dir, 'missing.exe'), wrong, artifacts], { encoding: 'utf8' });
    assert.equal(result.status, 2);
    assert.match(result.stderr, /SHA-256 differs/);
    await assert.rejects(readFile(join(artifacts, 'reference.json')), { code: 'ENOENT' });
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});
