// Reproducible offline corpus and per-fresh-process measurements.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { arch, release, platform } from 'node:os';
import { Script } from 'node:vm';

const self = fileURLToPath(import.meta.url);
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const [command, ...args] = process.argv.slice(2);

function wrappers(count, lines, heading = '') {
  const body = 'v=(v+1)^2;'.repeat(lines);
  return `${heading}(function(){var bias=3;${Array.from({ length: count }, (_, i) =>
    `function f${i}(v){${body}return v+bias;}`).join('')}return f0(4)===f${count - 1}(4);})()`;
}

function corpus() {
  const flat = Array.from({ length: 95_000 }, (_, i) => i % 397).join(',');
  const nested = Array.from({ length: 28 }, (_, i) => `function level${i}(){return `).join('')
    + '1' + ';}'.repeat(28);
  const sparse = 'ASCII characters 0-123456789'.repeat(370_000);
  return [
    ['only-ascii', `/*${sparse}*/true`, 'ok', false],
    ['one-bmp-near-start', `/*ł${sparse}*/true`, 'ok', false],
    ['one-astral-near-end', `/*${sparse}🙂*/true`, 'ok', false],
    ['distributed-unicode', `/*${('abcé漢🙂'.repeat(420_000))}*/true`, 'ok', false],
    ['wrappers-ascii', wrappers(900, 220), 'ok', true],
    ['wrappers-mixed', wrappers(900, 220, '/*ł🙂*/'), 'ok', true],
    ['flat-array', `const a=[${flat}]; a.length===95000;`, 'ok', false],
    ['nested-wrappers', `${nested}\ntrue;`, 'classification', false],
    ['dense-statements', `(function(){let a=0;${'a++;'.repeat(90_000)}return a===90000;})();`, 'ok', false],
    ['early-error', wrappers(225, 120, '"use strict";').replace('return f0(', 'function bad(a,a){}return f0('), 'SyntaxError', false],
  ];
}

async function fixtures(directory) {
  await mkdir(directory, { recursive: false });
  const cases = [];
  for (const [name, text, expected, evaluate] of corpus()) {
    const bytes = Buffer.from(text);
    assert.ok(bytes.length < 12 * 1024 * 1024);
    const file = `${name}.js`;
    await writeFile(join(directory, file), bytes, { flag: 'wx' });
    cases.push({ name, file, bytes: bytes.length, sha256: hash(bytes), expected, evaluate });
  }
  await writeFile(join(directory, 'manifest.json'), JSON.stringify({ version: 1, cases }, null, 2) + '\n', { flag: 'wx' });
}

function reference(mode, file) {
  return readFile(file, 'utf8').then(source => {
    global.gc();
    const memory = process.memoryUsage();
    const start = process.hrtime.bigint();
    const before = process.cpuUsage();
    let code = 'ok';
    let script;
    try {
      script = new Script(source);
      if (mode === 'eval' && script.runInNewContext({}, { timeout: 2000 }) !== true) code = 'WrongResult';
    } catch (error) {
      code = error.name;
    }
    const cpu = process.cpuUsage(before);
    const elapsed = Number((process.hrtime.bigint() - start) / 1000n);
    const after = process.memoryUsage();
    // Values are deltas around compile, not V8 frontend high-water marks.
    console.log(JSON.stringify({ code, wallUs: elapsed, cpuUs: cpu.user + cpu.system,
      heapUsedDelta: after.heapUsed - memory.heapUsed, rssDelta: after.rss - memory.rss,
      v8: process.versions.v8, retainedHandle: !!script }));
  });
}

function satin(stdout) {
  const [total, ...phases] = stdout.trim().split(/\r?\n/).map(line => line.split('\t'));
  assert.equal(total[0], 'total', stdout);
  const number = v => v === '-' ? null : Number(v);
  return {
    code: total[1], wallUs: number(total[2]), cpuUs: number(total[3]),
    rustPeakNetBytes: number(total[4]), rustRequestedBytes: number(total[5]),
    vmRetainedEstimateBytes: number(total[6]),
    phases: phases.map(([tag, name, wallUs, netBytes, peakNetBytes]) => {
      assert.equal(tag, 'phase');
      return { name, wallUs: +wallUs, netBytes: +netBytes, peakNetBytes: +peakNetBytes };
    }),
  };
}

async function run(executable, directory, destination, commit, repeats = '3') {
  executable = resolve(executable);
  directory = resolve(directory);
  const manifestBytes = await readFile(join(directory, 'manifest.json'));
  const manifest = JSON.parse(manifestBytes);
  const iterations = Number(repeats);
  assert.equal(manifest.version, 1);
  assert.ok(Number.isInteger(iterations) && iterations >= 1 && iterations <= 15);
  const report = { startedUtc: new Date().toISOString(), commit, iterations,
    probeSha256: hash(await readFile(executable)), manifestSha256: hash(manifestBytes),
    platform: platform(), osRelease: release(), arch: arch(), node: process.version, cases: [] };
  for (const entry of manifest.cases) {
    assert.match(entry.file, /^[a-z0-9-]+\.js$/);
    const file = join(directory, entry.file);
    const bytes = await readFile(file);
    assert.equal(bytes.length, entry.bytes);
    assert.equal(hash(bytes), entry.sha256);
    const samples = [];
    for (const mode of entry.evaluate ? ['compile', 'eval'] : ['compile']) {
      for (let repeat = 0; repeat < iterations; repeat++) {
        const options = { encoding: 'utf8', timeout: 30_000, windowsHide: true, maxBuffer: 4096 };
        // Run serially. A fresh process prevents warm caches from biasing a later sample.
        const rust = satin(execFileSync(executable, [mode, file], options));
        const v8 = JSON.parse(execFileSync(process.execPath, ['--expose-gc', self, 'v8', mode, file], options));
        samples.push({ mode, repeat, rust, v8 });
      }
    }
    report.cases.push({ ...entry, samples });
  }
  await writeFile(destination, JSON.stringify(report, null, 2) + '\n', { flag: 'wx' });
  console.log(destination);
}

// Rotate the first executable every round. Each variant spends the same
// number of samples in each position, including the cold first position.
async function paired(directory, destination, rounds, ...variants) {
  directory = resolve(directory);
  const count = Number(rounds);
  assert.ok(variants.length >= 4 && variants.length % 2 === 0);
  const probes = [];
  for (let i = 0; i < variants.length; i += 2) {
    const path = resolve(variants[i + 1]);
    probes.push({ label: variants[i], path, sha256: hash(await readFile(path)) });
  }
  assert.equal(new Set(probes.map(p => p.label)).size, probes.length);
  assert.ok(Number.isInteger(count) && count > 0 && count <= 24 && count % probes.length === 0);
  const bytes = await readFile(join(directory, 'manifest.json'));
  const manifest = JSON.parse(bytes);
  assert.equal(manifest.version, 1);
  const report = { startedUtc: new Date().toISOString(), platform: platform(), arch: arch(),
    osRelease: release(), node: process.version, manifestSha256: hash(bytes), probes, rounds: count,
    qaHeapBytes: process.env.SATIN_QA_HEAP_BYTES ? Number(process.env.SATIN_QA_HEAP_BYTES) : null,
    cases: [] };
  for (const entry of manifest.cases) {
    assert.match(entry.file, /^[a-z0-9-]+\.js$/);
    const file = join(directory, entry.file);
    const input = await readFile(file);
    assert.equal(input.length, entry.bytes);
    assert.equal(hash(input), entry.sha256);
    const samples = [];
    for (const mode of entry.evaluate ? ['compile', 'eval'] : ['compile']) {
      for (let round = 0; round < count; round++) {
        for (let pos = 0; pos < probes.length; pos++) {
          const probe = probes[(round + pos) % probes.length];
          const result = satin(execFileSync(probe.path, [mode, file], {
            encoding: 'utf8', timeout: 30000, windowsHide: true, maxBuffer: 4096,
          }));
          samples.push({ mode, round, pos, label: probe.label, ...result });
        }
      }
    }
    report.cases.push({ ...entry, samples });
  }
  await writeFile(destination, JSON.stringify(report, null, 2) + '\n', { flag: 'wx' });
  console.log(destination);
}

if (command === 'fixtures' && args.length === 1) {
  await fixtures(resolve(args[0]));
} else if (command === 'run' && [4, 5].includes(args.length)) {
  await run(...args);
} else if (command === 'v8' && args.length === 2) {
  await reference(...args);
} else if (command === 'paired' && args.length >= 7) {
  await paired(...args);
} else {
  throw new Error('fixtures <new-dir> | run <probe> <fixtures-dir> <new-json> <commit> [repeats] | paired <fixtures-dir> <new-json> <rounds> <label> <probe> ...');
}
