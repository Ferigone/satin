import { createHash } from 'node:crypto';
import { readFile, writeFile } from 'node:fs/promises';
import { pathToFileURL } from 'node:url';
import { crc32, deflateSync, inflateSync } from 'node:zlib';
import { comparisonCase } from './quiet-cinder-cases.mjs';

const SIGNATURE = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]);
const MAX_DIMENSION = 4096;
export const CHANNEL_THRESHOLD = 12;
export const MIN_REFERENCE_EDGE_PERCENT = 0.15;

function paeth(a, b, c) {
  const p = a + b - c;
  const da = Math.abs(p - a), db = Math.abs(p - b), dc = Math.abs(p - c);
  return da <= db && da <= dc ? a : db <= dc ? b : c;
}

export function decodePng(bytes) {
  if (!bytes.subarray(0, 8).equals(SIGNATURE)) throw new Error('not a PNG');
  let width, height, channels;
  const idat = [];
  let end = false;
  for (let offset = 8; offset + 12 <= bytes.length;) {
    const length = bytes.readUInt32BE(offset);
    const next = offset + length + 12;
    if (next > bytes.length) throw new Error('truncated PNG chunk');
    const type = bytes.toString('ascii', offset + 4, offset + 8);
    const data = bytes.subarray(offset + 8, next - 4);
    if (crc32(bytes.subarray(offset + 4, next - 4)) !== bytes.readUInt32BE(next - 4)) {
      throw new Error(`invalid ${type} CRC`);
    }
    if (offset === 8 && type !== 'IHDR') throw new Error('missing PNG header');
    if (type === 'IHDR') {
      if (offset !== 8 || length !== 13) throw new Error('invalid PNG header');
      width = data.readUInt32BE(0);
      height = data.readUInt32BE(4);
      if (!width || !height || width > MAX_DIMENSION || height > MAX_DIMENSION) {
        throw new Error('unsupported PNG dimensions');
      }
      channels = data[9] === 2 ? 3 : data[9] === 6 ? 4 : 0;
      if (data[8] !== 8 || !channels || data[10] || data[11] || data[12]) {
        throw new Error('expected non-interlaced 8-bit RGB or opaque RGBA PNG');
      }
    } else if (type === 'IDAT') {
      idat.push(data);
    } else if (type === 'IEND') {
      if (length || next !== bytes.length) throw new Error('invalid PNG end');
      end = true;
      break;
    } else if (['tRNS', 'acTL'].includes(type) || /^[A-Z]/.test(type) && type !== 'PLTE') {
      throw new Error(`unsupported PNG chunk ${type}`);
    }
    offset = next;
  }
  if (!end || !idat.length) throw new Error('incomplete PNG');
  const stride = width * channels, length = (stride + 1) * height;
  const raw = inflateSync(Buffer.concat(idat), { maxOutputLength: length });
  if (raw.length !== length) throw new Error('incorrect PNG scanline length');
  let row = Buffer.alloc(stride), prior = Buffer.alloc(stride);
  const rgb = Buffer.alloc(width * height * 3);
  for (let y = 0, j = 0; y < height; ++y) {
    const start = y * (stride + 1), filter = raw[start];
    if (filter > 4) throw new Error(`invalid PNG filter ${filter}`);
    for (let x = 0; x < stride; ++x) {
      const a = x < channels ? 0 : row[x - channels];
      const b = prior[x], c = x < channels ? 0 : prior[x - channels];
      const predictor = filter === 0 ? 0 : filter === 1 ? a : filter === 2 ? b :
        filter === 3 ? (a + b) >> 1 : paeth(a, b, c);
      row[x] = raw[start + 1 + x] + predictor;
      if (channels !== 4 || x % 4 !== 3) rgb[j++] = row[x];
      else if (row[x] !== 255) throw new Error('PNG contains non-opaque pixels; do not composite it');
    }
    [row, prior] = [prior, row];
  }
  return { width, height, rgb };
}

function chunk(type, data) {
  const bytes = Buffer.alloc(data.length + 12);
  bytes.writeUInt32BE(data.length);
  bytes.write(type, 4);
  data.copy(bytes, 8);
  bytes.writeUInt32BE(crc32(bytes.subarray(4, -4)), bytes.length - 4);
  return bytes;
}

function diffPng(reference, native) {
  const { width, height } = reference;
  const header = Buffer.alloc(13);
  header.writeUInt32BE(width);
  header.writeUInt32BE(height, 4);
  header.set([8, 2], 8);
  const raw = Buffer.alloc((width * 3 + 1) * height);
  for (let y = 0, i = 0; y < height; ++y) {
    for (let j = y * (width * 3 + 1) + 1, x = 0; x < width * 3; ++x, ++i) {
      raw[j + x] = Math.min(255, Math.abs(reference.rgb[i] - native.rgb[i]) * 4);
    }
  }
  return Buffer.concat([
    SIGNATURE, chunk('IHDR', header), chunk('IDAT', deflateSync(raw)), chunk('IEND', Buffer.alloc(0)),
  ]);
}

function adjacentDelta(rgb, a, b) {
  return Math.max(Math.abs(rgb[a] - rgb[b]), Math.abs(rgb[a + 1] - rgb[b + 1]),
    Math.abs(rgb[a + 2] - rgb[b + 2]));
}

export function measure(reference, native, rect) {
  if (reference.width !== native.width || reference.height !== native.height) {
    throw new Error('image dimensions differ; do not resize, scale, or align');
  }
  const [left, top, w, h] = rect;
  if (rect.length !== 4 || !rect.every(Number.isInteger) || left < 0 || top < 0 || w < 1 || h < 1 ||
      left + w > reference.width || top + h > reference.height) {
    throw new Error(`crop is outside image or invalid: ${rect}`);
  }
  const a = reference.rgb, b = native.rgb, stride = reference.width * 3;
  let absoluteSum = 0, changedPixels = 0, edgePixels = 0, maxChannelDelta = 0;
  let xMin = left + w, xMax = -1, yMin = top + h, yMax = -1;
  for (let y = top; y < top + h; ++y) {
    for (let x = left, i = y * stride + left * 3; x < left + w; ++x, i += 3) {
      const r = Math.abs(a[i] - b[i]), g = Math.abs(a[i + 1] - b[i + 1]);
      const blue = Math.abs(a[i + 2] - b[i + 2]), delta = Math.max(r, g, blue);
      absoluteSum += r + g + blue;
      maxChannelDelta = Math.max(maxChannelDelta, delta);
      if (delta >= CHANNEL_THRESHOLD) {
        ++changedPixels;
        xMin = Math.min(xMin, x); xMax = Math.max(xMax, x);
        yMin = Math.min(yMin, y); yMax = Math.max(yMax, y);
      }
      if ((x > left && adjacentDelta(a, i, i - 3) >= CHANNEL_THRESHOLD) ||
          (y > top && adjacentDelta(a, i, i - stride) >= CHANNEL_THRESHOLD)) ++edgePixels;
    }
  }
  const pixels = w * h;
  return {
    pixels, changedPixels,
    meanAbsoluteRgb: absoluteSum / (pixels * 3),
    changedPercent: 100 * changedPixels / pixels,
    maxChannelDelta,
    referenceEdgePercent: 100 * edgePixels / pixels,
    changedBounds: changedPixels ? [xMin, yMin, xMax - xMin + 1, yMax - yMin + 1] : null,
  };
}

export function compare(spec, reference, native) {
  for (const [name, image] of [['reference', reference], ['native', native]]) {
    if (image.width !== spec.size[0] || image.height !== spec.size[1]) {
      throw new Error(`${name} is ${image.width}×${image.height}; expected ${spec.size.join('×')} client pixels`);
    }
  }
  const crops = spec.crops.map(crop => {
    const measures = measure(reference, native, crop.rect);
    return {
      ...crop, ...measures,
      status: measures.referenceEdgePercent < MIN_REFERENCE_EDGE_PERCENT ? 'reference-too-flat' :
        measures.meanAbsoluteRgb > crop.maxMeanRgb || measures.changedPercent > crop.maxChangedPercent
          ? 'outside-budgets' : 'within-budgets',
    };
  });
  const status = crops.some(crop => crop.status === 'reference-too-flat') ? 'invalid-reference' :
    crops.some(crop => crop.status === 'outside-budgets') ? 'outside-budgets' : 'within-budgets';
  return { status, crops, wholeFrame: measure(reference, native, [0, 0, ...spec.size]) };
}

async function main(args) {
  if (args.length < 3 || args.length > 4) {
    throw new Error('usage: node tools/quiet-cinder-compare.mjs CASE CHROMIUM.png SATIN.png [DIFF.png]');
  }
  const [name, refPath, nativePath, diffPath] = args;
  const spec = comparisonCase(name);
  const [refBytes, nativeBytes] = await Promise.all([readFile(refPath), readFile(nativePath)]);
  const reference = decodePng(refBytes), native = decodePng(nativeBytes);
  const result = compare(spec, reference, native);
  if (diffPath) await writeFile(diffPath, diffPng(reference, native), { flag: 'wx' });
  const sha256 = bytes => createHash('sha256').update(bytes).digest('hex');
  console.log(JSON.stringify({
    case: name, size: spec.size, dataScope: spec.dataScope, channelThreshold: CHANNEL_THRESHOLD,
    minReferenceEdgePercent: MIN_REFERENCE_EDGE_PERCENT,
    reference: { path: refPath, sha256: sha256(refBytes) },
    native: { path: nativePath, sha256: sha256(nativeBytes) },
    ...result,
  }, null, 2));
  return { 'within-budgets': 0, 'outside-budgets': 1, 'invalid-reference': 2 }[result.status];
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    process.exitCode = await main(process.argv.slice(2));
  } catch (error) {
    console.error(error.message);
    process.exitCode = 2;
  }
}
