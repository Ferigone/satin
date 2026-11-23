import { constants } from 'node:fs';
import { open, stat } from 'node:fs/promises';

const MAX_BYTES = 16 * 1024 * 1024;
const MAX_SECTIONS = 1024;
const MAX_ENTRIES = 100_000;
const MAX_DESCRIPTORS = 8192;
const MAX_SIGNATURE_VALUES = 65_536;
const PAGE_BYTES = 65_536;

// Binary section order differs from numeric ID order for tag and data count.
const SECTION_ORDER = [1, 2, 3, 4, 5, 13, 6, 7, 8, 9, 12, 10, 11];
const SECTION_NAMES = [
  'custom', 'type', 'import', 'function', 'table', 'memory', 'global', 'export',
  'start', 'element', 'code', 'data', 'data-count', 'tag',
];
const EXTERNAL_KINDS = ['function', 'table', 'memory', 'global', 'tag'];
const VALUE_TYPES = new Map([
  [0x7f, 'i32'], [0x7e, 'i64'], [0x7d, 'f32'], [0x7c, 'f64'], [0x7b, 'v128'],
  [0x70, 'funcref'], [0x6f, 'externref'],
]);

class InventoryError extends Error {}

function fail(kind, detail) {
  throw new InventoryError(`${kind}: ${detail}`);
}

class Reader {
  constructor(bytes) {
    this.bytes = bytes;
    this.at = 0;
  }

  take(length) {
    if (length > this.bytes.length - this.at) {
      fail('malformed', 'truncated field or section');
    }
    const start = this.at;
    this.at += length;
    return this.bytes.subarray(start, this.at);
  }

  byte() {
    if (this.done) fail('malformed', 'truncated field or section');
    return this.bytes[this.at++];
  }

  u32() {
    let result = 0;
    for (let shift = 0; shift <= 28; shift += 7) {
      const byte = this.byte();
      if (shift === 28 && byte > 0x0f) fail('malformed', 'invalid u32 LEB128');
      result += (byte & 0x7f) * 2 ** shift;
      if (byte < 0x80) return result;
    }
  }

  count(maximum = MAX_ENTRIES) {
    const value = this.u32();
    if (value > maximum) fail('limit', `vector exceeds ${maximum} entries`);
    return value;
  }

  get done() {
    return this.at === this.bytes.length;
  }

  requireDone() {
    if (!this.done) fail('malformed', 'trailing bytes in a metadata section');
  }

  skipName() {
    // No name strings are emitted; the host validation checks their encoding.
    this.take(this.u32());
  }
}

function valueType(reader) {
  const type = VALUE_TYPES.get(reader.byte());
  if (!type) fail('unsupported', 'value type outside the inventory profile');
  return type;
}

function externalKind(reader) {
  const kind = EXTERNAL_KINDS[reader.byte()];
  if (!kind) fail('unsupported', 'external kind outside the inventory profile');
  return kind;
}

function limits(reader, isMemory) {
  const flags = reader.byte();
  if (flags !== 0 && flags !== 1 && !(isMemory && flags === 3)) {
    fail('unsupported', `${isMemory ? 'memory' : 'table'} limits outside the inventory profile`);
  }
  const initial = reader.u32();
  const maximum = flags & 1 ? reader.u32() : null;
  return { initial, maximum, shared: Boolean(flags & 2) };
}

function table(reader, source) {
  const elementType = valueType(reader);
  if (elementType !== 'funcref' && elementType !== 'externref') {
    fail('malformed', 'table element type is not a reference');
  }
  const { initial, maximum } = limits(reader, false);
  return { source, elementType, initialElements: initial, maximumElements: maximum };
}

function memory(reader, source) {
  const { initial, maximum, shared } = limits(reader, true);
  return {
    source, addressType: 'i32', shared, pageBytes: PAGE_BYTES,
    initialPages: initial, maximumPages: maximum,
    initialBytes: initial * PAGE_BYTES, maximumBytes: maximum === null ? null : maximum * PAGE_BYTES,
  };
}

function kindCounts() {
  return Object.fromEntries(EXTERNAL_KINDS.map(kind => [kind, 0]));
}

function recordFunctionType(reader, types, source) {
  const type = types[reader.u32()];
  if (!type) fail('malformed', 'function references an unknown type');
  type[source]++;
}

function satinComparison(report) {
  const observedGaps = [];
  const note = (condition, gap) => { if (condition) observedGaps.push(gap); };
  const { types, defined } = report.functions;
  const imports = report.imports.byKind;
  note(report.sourceBytes > 2 * 1024 * 1024, 'source-byte-limit');
  for (const { name, entries } of report.sections.order) {
    note(!['custom', 'type', 'import', 'function', 'export', 'code'].includes(name), `${name}-section`);
    if (['type', 'import', 'function', 'export'].includes(name) && entries > 4096) {
      observedGaps.push(`${name}-entry-limit`);
    }
  }
  note(types.some(type => type.parameters.length > 64), 'parameter-limit');
  note(types.some(type => type.results.length > 1), 'multi-value-result');
  note(types.some(type => type.parameters.some(value => value !== 'i32')
    || type.results.some(value => value !== 'i32')), 'non-i32-signature');
  note(report.imports.count > imports.function, 'non-function-import');
  note(report.exports.count > report.exports.byKind.function, 'non-function-export');
  note(report.memories.some(memory => memory.shared), 'shared-memory');
  note(report.memories.length > 1, 'multiple-memories');
  note(report.tables.length > 1, 'multiple-tables');
  return {
    baseline: 'cd96dd68eaa77a4547b711cb5ddac0389c8f3082',
    observedGaps,
    notInspected: defined === 0 ? [] : ['instructions, locals, operand stack and execution budgets'],
  };
}

function inventory(bytes) {
  const reader = new Reader(bytes);
  if (!reader.take(4).equals(Buffer.from([0x00, 0x61, 0x73, 0x6d]))) {
    fail('unsupported', 'expected a raw Wasm binary (wrong magic)');
  }
  const version = reader.take(4).readUInt32LE();
  if (version !== 1) fail('unsupported', 'expected core Wasm binary version 1');

  const report = {
    format: 'wasm-core-v1',
    sourceBytes: bytes.length,
    sections: { counts: { total: 0, standard: 0, custom: 0 }, order: [] },
    functions: { imported: 0, defined: 0, codeBodies: 0, types: [] },
    imports: { count: 0, byKind: kindCounts(), globals: [], tags: [] },
    exports: { count: 0, byKind: kindCounts() },
    tables: [],
    memories: [],
  };
  const { sections, functions, imports, exports, tables, memories } = report;
  let previous = -1;
  let signatureValuesLeft = MAX_SIGNATURE_VALUES;
  function readSignatureValues(section) {
    const count = section.count(signatureValuesLeft);
    signatureValuesLeft -= count;
    return Array.from({ length: count }, () => valueType(section));
  }

  while (!reader.done) {
    if (++sections.counts.total > MAX_SECTIONS) fail('limit', `over ${MAX_SECTIONS} sections`);
    const id = reader.byte();
    const byteLength = reader.u32();
    const section = new Reader(reader.take(byteLength));
    const summary = { name: SECTION_NAMES[id], payloadBytes: byteLength };
    sections.order.push(summary);
    if (id === 0) {
      sections.counts.custom++;
      section.skipName();
      continue; // Custom payloads are opaque.
    }
    const rank = SECTION_ORDER.indexOf(id);
    if (rank < 0) fail('unsupported', `unknown section ID ${id}`);
    if (rank <= previous) fail('malformed', 'duplicate or out-of-order standard section');
    previous = rank;
    sections.counts.standard++;
    if (id === 8 || id === 12) {
      summary[id === 8 ? 'functionIndex' : 'declaredSegments'] = section.u32();
      section.requireDone();
      continue;
    }
    const count = section.count([1, 2, 4, 5, 7].includes(id) ? MAX_DESCRIPTORS : MAX_ENTRIES);
    summary.entries = count;
    switch (id) {
      case 1:
        for (let i = 0; i < count; i++) {
          if (section.byte() !== 0x60) fail('unsupported', 'type is not a plain function signature');
          functions.types.push({
            parameters: readSignatureValues(section), results: readSignatureValues(section),
            imported: 0, defined: 0,
          });
        }
        break;
      case 2:
        imports.count = count;
        for (let i = 0; i < count; i++) {
          section.skipName(); // module
          section.skipName(); // field
          const kind = externalKind(section);
          imports.byKind[kind]++;
          switch (kind) {
            case 'function': recordFunctionType(section, functions.types, 'imported'); break;
            case 'table': tables.push(table(section, 'import')); break;
            case 'memory': memories.push(memory(section, 'import')); break;
            case 'global': {
              const type = valueType(section);
              const mutability = section.byte();
              if (mutability > 1) fail('unsupported', 'global mutability outside the inventory profile');
              imports.globals.push({ valueType: type, mutable: mutability === 1 });
              break;
            }
            case 'tag':
              if (section.byte() !== 0) fail('unsupported', 'tag attribute outside the inventory profile');
              imports.tags.push({ typeIndex: section.u32() });
              break;
          }
        }
        functions.imported = imports.byKind.function;
        break;
      case 3:
        functions.defined = count;
        for (let i = 0; i < count; i++) recordFunctionType(section, functions.types, 'defined');
        break;
      case 4:
        for (let i = 0; i < count; i++) tables.push(table(section, 'definition'));
        break;
      case 5:
        for (let i = 0; i < count; i++) memories.push(memory(section, 'definition'));
        break;
      case 7:
        exports.count = count;
        for (let i = 0; i < count; i++) {
          section.skipName();
          exports.byKind[externalKind(section)]++;
          section.u32(); // index
        }
        break;
      case 10:
        functions.codeBodies = count;
        continue; // Count only; host validation checks complete bodies.
      default:
        continue; // Global, element, data and tag sections are count-only.
    }
    section.requireDone();
  }

  // Static validation only: never compile a Module object or instantiate it.
  if (!WebAssembly.validate(bytes)) {
    fail('validation', 'Node rejected the Wasm module (malformed or unsupported by this runtime)');
  }
  report.validation = { host: 'Node WebAssembly.validate', nodeVersion: process.version };
  report.satinCheckpoint = satinComparison(report);
  return report;
}

async function readLocalFile(path) {
  if (/^[\\/]{2}|^[a-z][a-z0-9+.-]*:\/\//i.test(path)) {
    fail('input', 'URLs and UNC paths are not accepted');
  }
  if (!(await stat(path)).isFile()) fail('input', 'expected a regular file');
  const file = await open(path, constants.O_RDONLY | (constants.O_NONBLOCK ?? 0));
  try {
    const info = await file.stat();
    if (!info.isFile()) fail('input', 'expected a regular file');
    if (info.size > MAX_BYTES) fail('limit', `file exceeds ${MAX_BYTES} bytes`);
    // Read at most one extra byte to detect growth after stat.
    const bytes = Buffer.alloc(info.size + 1);
    let length = 0;
    while (length < bytes.length) {
      const { bytesRead } = await file.read(bytes, length, bytes.length - length, length);
      if (bytesRead === 0) break;
      length += bytesRead;
    }
    if (length !== info.size) fail('input', 'file size changed during read');
    return bytes.subarray(0, length);
  } finally {
    await file.close();
  }
}

try {
  if (process.argv.length !== 3 || process.argv[2].startsWith('-')) {
    fail('usage', 'node tools/wasm-compat/inventory.mjs <local.wasm>');
  }
  const report = inventory(await readLocalFile(process.argv[2]));
  process.stdout.write(`${JSON.stringify(report, null, 2)}\n`);
} catch (error) {
  const message = error instanceof InventoryError ? error.message : `could not inventory file (${error.code ?? 'unexpected error'})`;
  process.stderr.write(`wasm inventory: ${message}\n`);
  process.exitCode = 1;
}
