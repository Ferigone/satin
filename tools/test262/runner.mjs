import { readFile, mkdir, writeFile } from 'node:fs/promises';
import { resolve, relative, sep, isAbsolute, posix } from 'node:path';
import { createHash } from 'node:crypto';
import { spawn, execFileSync } from 'node:child_process';
import { createInterface } from 'node:readline';
import { pathToFileURL } from 'node:url';
import YAML from 'yaml';

export const PINNED_REVISION = '48221d7c1f7745dc13e420a2e850a248c0b2afb4';

function under(root, path) {
  const rel = relative(root, path);
  if (rel === '..' || rel.startsWith(`..${sep}`) || isAbsolute(rel)) throw new Error(`path escapes root: ${path}`);
  return rel.replaceAll(sep, '/');
}

export function metadata(source) {
  const match = /\/\*---\s*\r?\n([\s\S]*?)\r?\n---\*\//.exec(source);
  if (!match) throw new Error('missing Test262 YAML frontmatter');
  const parsed = YAML.parse(match[1], { maxAliasCount: 100, uniqueKeys: true });
  if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) throw new Error('frontmatter must be a mapping');
  for (const key of ['flags', 'includes', 'features']) {
    if (parsed[key] !== undefined && (!Array.isArray(parsed[key]) || parsed[key].some(item => typeof item !== 'string'))) {
      throw new Error(`${key} must be an array of strings`);
    }
    parsed[key] ??= [];
  }
  if (parsed.flags.includes('noStrict') && parsed.flags.includes('onlyStrict')) throw new Error('conflicting strictness flags');
  if (parsed.flags.includes('raw') && (parsed.flags.includes('onlyStrict') || parsed.includes.length)) throw new Error('raw test cannot use includes/onlyStrict');
  if (parsed.negative) {
    if (!['parse', 'early', 'runtime', 'resolution'].includes(parsed.negative.phase) || typeof parsed.negative.type !== 'string') {
      throw new Error('unsupported negative-test metadata shape');
    }
  }
  return parsed;
}

export function variants(meta) {
  if (meta.flags.includes('module')) return ['module'];
  if (meta.flags.includes('raw')) return ['raw'];
  if (meta.flags.includes('onlyStrict')) return ['strict'];
  if (meta.flags.includes('noStrict')) return ['sloppy'];
  return ['sloppy', 'strict'];
}

export function unsupportedReason(meta, variant) {
  if (meta.flags.some(flag => flag.startsWith('CanBlockIs'))) return 'agent/blocking host is not implemented';
  if (meta.negative?.phase === 'resolution' && variant !== 'module') return 'resolution phase requires the module flag';
  return null;
}

export function classify(meta, response) {
  if (!response || !['ok', 'error'].includes(response.outcome)) return { status: 'harness_error', reason: 'invalid worker response' };
  if (['protocol', 'host', 'setup'].includes(response.phase)) return { status: 'harness_error', reason: `worker ${response.phase} failed` };
  const kind = response.error?.kind;
  if (kind === 'UnsupportedFeature') return { status: 'unsupported', reason: response.error.message };
  if (kind === 'ExecutionLimit' || kind === 'HeapLimit') return { status: 'resource_limit', reason: kind };
  if (kind === 'AsyncIncomplete') return { status: 'async_incomplete', reason: response.error.message };
  if (kind === 'AsyncTestFailure') return { status: 'fail', reason: response.error.message };
  const negative = meta.negative;
  if (negative) {
    const phase = ['parse', 'early'].includes(negative.phase) ? 'parse' : negative.phase;
    if (response.outcome === 'error' && response.phase === phase && kind === negative.type) return { status: 'pass' };
    return { status: 'fail', reason: `expected ${negative.type} during ${phase}, got ${response.outcome} ${kind ?? ''} during ${response.phase}` };
  }
  if (response.outcome === 'ok' && response.phase === 'runtime') return { status: 'pass' };
  return { status: 'fail', reason: `unexpected ${kind ?? response.outcome} during ${response.phase}` };
}

// One bounded request at a time. The Rust process creates a new VM/realm for each.
// No JavaScript supplied by a test is run by this Node process.
export class Worker {
  constructor(executable, timeoutMs) {
    this.executable = executable;
    this.timeoutMs = timeoutMs;
    this.child = null;
    this.pending = null;
    this.stderr = '';
  }
  start() {
    const child = spawn(this.executable, ['--worker'], { windowsHide: true, stdio: ['pipe', 'pipe', 'pipe'] });
    this.child = child;
    this.stderr = '';
    const lines = createInterface({ input: child.stdout, crlfDelay: Infinity });
    lines.on('line', line => {
      if (this.child !== child || !this.pending) return;
      const pending = this.pending;
      this.pending = null;
      clearTimeout(pending.timer);
      try {
        const result = JSON.parse(line);
        if (result.id !== pending.id) throw new Error('worker response id mismatch');
        pending.resolve(result);
      } catch (error) {
        this.stop();
        pending.reject({ status: 'harness_error', reason: String(error) });
      }
    });
    child.stderr.on('data', chunk => { this.stderr = (this.stderr + chunk).slice(-8192); });
    const died = reason => {
      if (this.child !== child) return;
      this.child = null;
      if (this.pending) {
        const pending = this.pending;
        this.pending = null;
        clearTimeout(pending.timer);
        pending.reject({ status: 'crash', reason, stderr: this.stderr });
      }
    };
    child.on('error', error => died(String(error)));
    child.on('exit', (code, signal) => died(`worker exited ${code ?? signal}`));
    child.stdin.on('error', error => died(String(error)));
  }
  request(request) {
    if (this.pending) throw new Error('worker already has a pending request');
    if (!this.child) this.start();
    return new Promise((resolveRequest, reject) => {
      const timer = setTimeout(() => {
        this.pending = null;
        this.stop();
        reject({ status: 'timeout', reason: `exceeded ${this.timeoutMs} ms` });
      }, this.timeoutMs);
      this.pending = { id: request.id, timer, resolve: resolveRequest, reject };
      this.child.stdin.write(`${JSON.stringify(request)}\n`);
    });
  }
  stop() {
    const child = this.child;
    this.child = null;
    // Kill only the worker process created by this runner, never a browser/user process.
    if (child) { child.stdin.destroy(); child.kill(); }
  }
}

function git(root, args) {
  return execFileSync('git', ['-C', root, ...args], { encoding: 'utf8', windowsHide: true, maxBuffer: 32 * 1024 * 1024 });
}

function blobHash(bytes) {
  return createHash('sha1').update(`blob ${bytes.length}\0`).update(bytes).digest('hex');
}

// Read immutable Git objects, not the Windows checkout: core.autocrlf and
// filesystem normalization must never rewrite the ECMAScript test input.
class GitBlobs {
  constructor(root) { this.root = root; this.child = null; this.buffer = Buffer.alloc(0); this.pending = null; }
  start() {
    this.child = spawn('git', ['-C', this.root, 'cat-file', '--batch'], { windowsHide: true, stdio: ['pipe', 'pipe', 'pipe'] });
    this.child.stdout.on('data', chunk => {
      this.buffer = Buffer.concat([this.buffer, chunk]);
      if (!this.pending) return;
      const end = this.buffer.indexOf(10);
      if (end === -1) return;
      const header = this.buffer.subarray(0, end).toString('ascii').split(' ');
      const length = Number(header[2]);
      if (header[0] !== this.pending.hash || header[1] !== 'blob' || !Number.isSafeInteger(length) || length > 16 * 1024 * 1024) {
        const pending = this.pending; this.pending = null;
        pending.reject(new Error(`invalid Git blob reply: ${header.join(' ')}`)); return;
      }
      if (this.buffer.length < end + 1 + length + 1) return;
      const bytes = this.buffer.subarray(end + 1, end + 1 + length);
      const pending = this.pending; this.pending = null;
      this.buffer = this.buffer.subarray(end + 1 + length + 1);
      if (blobHash(bytes) !== pending.hash) pending.reject(new Error('Git blob hash mismatch'));
      else {
        try { pending.resolve(new TextDecoder('utf-8', { fatal: true, ignoreBOM: true }).decode(bytes)); }
        catch (error) { pending.reject(error); }
      }
    });
    const fail = error => { if (this.pending) { const pending = this.pending; this.pending = null; pending.reject(error); } };
    this.child.on('error', fail);
    this.child.on('exit', code => fail(new Error(`git cat-file exited ${code}`)));
    this.child.stdin.on('error', fail);
    this.child.stderr.resume();
  }
  read(hash) {
    if (this.pending) throw new Error('concurrent Git blob request');
    if (!this.child) this.start();
    return new Promise((resolve, reject) => { this.pending = { hash, resolve, reject }; this.child.stdin.write(`${hash}\n`); });
  }
  close() { if (this.child) this.child.stdin.end(); }
}

export function moduleFixturePath(referrer, units) {
  if (!Array.isArray(units) || units.length > 8192 || units.some(unit => !Number.isInteger(unit) || unit < 0 || unit > 0xffff)) {
    throw { status: 'harness_error', reason: 'invalid module specifier from worker' };
  }
  const specifier = String.fromCharCode(...units);
  if (!(specifier.startsWith('./') || specifier.startsWith('../')) || !specifier.isWellFormed() || /[\\\0]/u.test(specifier)) {
    throw { status: 'unsupported', reason: 'Test262 host supports relative source-text module fixtures' };
  }
  const path = posix.normalize(posix.join(posix.dirname(referrer), specifier));
  if (!path.startsWith('test/')) {
    throw { status: 'harness_error', reason: 'module fixture path escapes the pinned test tree' };
  }
  if (path.endsWith('.json')) {
    throw { status: 'unsupported', reason: 'JSON module fixtures are not implemented' };
  }
  return path;
}

async function moduleGraph(worker, rootPath, request, readCanonical) {
  const parse = (id, source) => worker.request({
    id, source, module_goal: true, parse_only: true, max_instructions: request.max_instructions,
  });
  const root = await parse(request.id, request.source);
  if (root.outcome !== 'ok') return { response: root };

  const files = [];
  const edges = [];
  const parsed = new Map([[rootPath, root]]);
  const frames = [{ path: rootPath, requests: root.module_requests, next: 0 }];
  let bytes = Buffer.byteLength(request.source);
  while (frames.length) {
    const frame = frames.at(-1);
    if (!Array.isArray(frame.requests)) throw { status: 'harness_error', reason: 'worker did not return module requests' };
    if (frame.next === frame.requests.length) { frames.pop(); continue; }
    if (edges.length >= 16384) throw { status: 'resource_limit', reason: 'module fixture edge count exceeds 16384' };
    const specifier = frame.requests[frame.next++];
    const target = moduleFixturePath(frame.path, specifier);
    const identifier = target === rootPath ? request.id : target;
    edges.push({ referrer: frame.path === rootPath ? request.id : frame.path, specifier, target: identifier });
    if (parsed.has(target)) continue;
    if (files.length >= 1023) throw { status: 'resource_limit', reason: 'module fixture count exceeds 1023' };
    const source = await readCanonical(target);
    bytes += Buffer.byteLength(source);
    if (bytes > 8 * 1024 * 1024) throw { status: 'resource_limit', reason: 'module fixture source bytes exceed 8 MiB' };
    files.push({ identifier, source });
    const reply = await parse(target, source);
    parsed.set(target, reply);
    // Keep the original fixture in the final request. The link operation must
    // observe its parse failure at the point it resolves that dependency.
    if (reply.outcome === 'ok') frames.push({ path: target, requests: reply.module_requests, next: 0 });
    else break;
  }
  return { module_files: files, module_edges: edges };
}

export async function run(options) {
  const root = resolve(options.test262);
  const revision = git(root, ['rev-parse', 'HEAD']).trim();
  if (revision !== PINNED_REVISION) throw new Error(`expected Test262 ${PINNED_REVISION}, got ${revision}`);
  const tree = git(root, ['ls-tree', '-r', '-z', 'HEAD']);
  const hashes = new Map(tree.split('\0').filter(Boolean).map(entry => {
    const [description, path] = entry.split('\t');
    return [path, description.split(' ')[2]];
  }));
  const blobs = new GitBlobs(root);
  const readCanonical = async name => {
    const full = resolve(root, name);
    const path = under(root, full);
    if (!hashes.has(path)) throw new Error(`untracked/missing upstream fixture: ${path}`);
    return blobs.read(hashes.get(path));
  };
  const selected = new Set();
  for (const name of options.paths) {
    const path = resolve(root, 'test', name);
    const rel = under(root, path);
    under(resolve(root, 'test'), path);
    if (name.endsWith('.js')) {
      if (!hashes.has(rel)) throw new Error(`missing test: ${rel}`);
      selected.add(rel);
    } else {
      const prefix = `${rel.replace(/\/$/, '')}/`;
      const found = [...hashes.keys()].filter(file => file.startsWith(prefix) && file.endsWith('.js') && !file.split('/').at(-1).includes('_FIXTURE'));
      if (!found.length) throw new Error(`empty/missing test scope: ${rel}`);
      for (const file of found) selected.add(file);
    }
  }
  const allFiles = [...selected].sort();
  const files = allFiles.slice(options.offset, options.limit === null ? undefined : options.offset + options.limit);
  const setupCache = new Map();
  const setupFile = async name => {
    if (!setupCache.has(name)) {
      under(resolve(root, 'harness'), resolve(root, 'harness', name));
      setupCache.set(name, { name, source: await readCanonical(`harness/${name}`) });
    }
    return setupCache.get(name);
  };
  const dynamicBundleCache = new Map();
  const dynamicSuite = 'test/language/expressions/dynamic-import';
  const dynamicBundle = async path => {
    const prefix = path.startsWith(`${dynamicSuite}/`) ? dynamicSuite : posix.dirname(path);
    if (!dynamicBundleCache.has(prefix)) {
      const sources = [];
      let bytes = 0;
      for (const identifier of hashes.keys()) {
        if (!identifier.startsWith(`${prefix}/`) || !identifier.endsWith('.js') || !identifier.split('/').at(-1).includes('_FIXTURE')) continue;
        if (sources.length === 1023) throw { status: 'resource_limit', reason: 'dynamic fixture count exceeds 1023' };
        const source = await readCanonical(identifier);
        bytes += Buffer.byteLength(source);
        if (bytes > 4 * 1024 * 1024) throw { status: 'resource_limit', reason: 'dynamic fixture source bytes exceed 4 MiB' };
        sources.push({ identifier, source });
      }
      dynamicBundleCache.set(prefix, sources);
    }
    return dynamicBundleCache.get(prefix);
  };
  const worker = new Worker(resolve(options.worker), options.timeoutMs);
  const results = [];
  const start = Date.now();
  try {
    for (let index = 0; index < files.length; index++) {
      const path = files[index];
      const source = await readCanonical(path);
      let meta;
      try { meta = metadata(source); }
      catch (error) { results.push({ path, variant: null, status: 'invalid_metadata', reason: String(error) }); continue; }
      for (const variant of variants(meta)) {
        const base = { path, variant, features: meta.features };
        const unsupported = unsupportedReason(meta, variant);
        if (unsupported) { results.push({ ...base, status: 'unsupported', reason: unsupported }); continue; }
        const parseOnly = ['parse', 'early'].includes(meta.negative?.phase);
        const setup = [];
        if (!parseOnly && variant !== 'raw') {
          const names = ['sta.js', 'assert.js', ...(meta.flags.includes('async') ? ['doneprintHandle.js'] : []), ...meta.includes];
          for (const name of names) setup.push(await setupFile(name));
        }
        const request = { id: `${path}#${variant}`, source, setup, strict: variant === 'strict', parse_only: parseOnly, async_test: meta.flags.includes('async'), max_instructions: options.maxInstructions };
        try {
          if (variant === 'module') {
            request.module_goal = true;
            request.link_only = meta.negative?.phase === 'resolution';
          }
          let response;
          if (request.module_goal && !parseOnly) {
            const graph = await moduleGraph(worker, path, request, readCanonical);
            if (graph.response) response = graph.response;
            else Object.assign(request, graph);
          }
          if (!response && !parseOnly && meta.features.some(feature => feature === 'dynamic-import' || feature === 'top-level-await')) {
            request.dynamic_files = await dynamicBundle(path);
          }
          response ??= await worker.request(request);
          results.push({ ...base, ...classify(meta, response), response });
        } catch (failure) {
          results.push({ ...base, status: failure.status ?? 'harness_error', reason: failure.reason ?? String(failure), stderr: failure.stderr });
        }
      }
      if ((index + 1) % 250 === 0) process.stderr.write(`${index + 1}/${files.length} files\n`);
    }
  } finally { worker.stop(); blobs.close(); }
  const counts = {};
  for (const result of results) counts[result.status] = (counts[result.status] ?? 0) + 1;
  const workerBytes = await readFile(resolve(options.worker));
  const report = {
    schema: 1, engine: 'independent Rust Browser JavaScript VM',
    test262Revision: revision, fixtureBytesVerifiedAgainstGit: true, fixtureSource: 'immutable Git blobs; checkout EOL settings ignored',
    workerSha256: createHash('sha256').update(workerBytes).digest('hex'),
    timestampUtc: new Date().toISOString(), durationMs: Date.now() - start,
    selection: { paths: options.paths, offset: options.offset, limit: options.limit, availableFiles: allFiles.length, testedFiles: files.length },
    units: 'test variants; default scripts run sloppy and strict, modules run once',
    counts, totalVariants: results.length,
    // All selected variants stay in the denominator, including missing features and harness failures.
    passFractionSelected: results.length ? (counts.pass ?? 0) / results.length : 0,
    limitations: ['This is a selected scope, not a full Test262 conformance score.', 'Source-text modules and dynamic imports use pinned relative fixtures. Top-level await uses VM jobs. Import attributes and agent hosts remain unsupported. Async completion supports VM jobs; external timers/worker hosts remain missing.', 'Upstream harness code, including doneprintHandle.js for async tests, is executed unchanged by the Rust VM.'],
    results,
  };
  const output = resolve(options.output);
  await mkdir(resolve(output, '..'), { recursive: true });
  await writeFile(output, `${JSON.stringify(report, null, 2)}\n`);
  process.stdout.write(`${JSON.stringify({ report: output, revision, counts, totalVariants: results.length, testedFiles: files.length, durationMs: report.durationMs })}\n`);
  return report;
}

export function parseArgs(args) {
  const options = { test262: null, worker: null, output: 'target/test262/report.json', paths: [], offset: 0, limit: null, timeoutMs: 3000, maxInstructions: 1_000_000 };
  const names = { '--test262': 'test262', '--worker': 'worker', '--output': 'output', '--offset': 'offset', '--limit': 'limit', '--timeout-ms': 'timeoutMs', '--instructions': 'maxInstructions' };
  for (let at = 0; at < args.length; at++) {
    const flag = args[at];
    const value = args[++at];
    if (value === undefined) throw new Error(`missing value for ${flag}`);
    if (flag === '--path') options.paths.push(value);
    else if (names[flag]) {
      const key = names[flag];
      options[key] = ['offset', 'limit', 'timeoutMs', 'maxInstructions'].includes(key) ? Number(value) : value;
    } else throw new Error(`unknown option: ${flag}`);
  }
  if (!options.test262 || !options.worker || !options.paths.length) throw new Error('required: --test262 DIR --worker EXE --path test-relative-path [--path ...]');
  for (const key of ['offset', 'timeoutMs', 'maxInstructions']) {
    if (!Number.isSafeInteger(options[key]) || options[key] < (key === 'offset' ? 0 : 1)) throw new Error(`invalid ${key}`);
  }
  if (options.limit !== null && (!Number.isSafeInteger(options.limit) || options.limit < 1)) throw new Error('invalid limit');
  return options;
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  run(parseArgs(process.argv.slice(2))).then(report => {
    if (Object.keys(report.counts).some(status => status !== 'pass')) process.exitCode = 1;
  }).catch(error => { process.stderr.write(`${error.stack ?? error}\n`); process.exitCode = 2; });
}
