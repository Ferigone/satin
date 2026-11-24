import { createHash } from 'node:crypto';
import { readFile, writeFile } from 'node:fs/promises';
import { gzipSync, gunzipSync } from 'node:zlib';

const statuses = ['pass', 'fail', 'unsupported', 'resource_limit', 'async_incomplete',
  'harness_error', 'timeout', 'crash', 'invalid_metadata'];
const variants = new Set(['sloppy', 'strict', 'raw', 'module']);

function identity(report) {
  const { test262Revision, runnerSha256, workerSha256 } = report;
  const { timeoutMs, maxInstructions } = report.limits ?? {};
  if (report.schema !== 1 || !/^[a-f0-9]{40}$/.test(test262Revision) ||
      !/^[a-f0-9]{64}$/.test(runnerSha256) || !/^[a-f0-9]{64}$/.test(workerSha256) ||
      !Number.isSafeInteger(timeoutMs) || timeoutMs < 1 ||
      !Number.isSafeInteger(maxInstructions) || maxInstructions < 1) {
    throw new Error('expected a schema-1 runner report with revision, both hashes and budgets');
  }
  return { test262Revision, runnerSha256, workerSha256, limits: { timeoutMs, maxInstructions } };
}

function collect(files, report) {
  if (!Array.isArray(report.results) || report.results.length !== report.totalVariants) {
    throw new Error('result row count differs from totalVariants');
  }
  const paths = new Set();
  for (const { path, variant, status } of report.results) {
    if (typeof path !== 'string' || !path.startsWith('test/') || !path.endsWith('.js') || /[\t\r\n]/.test(path) ||
        !statuses.includes(status) || (status === 'invalid_metadata' ? variant !== null : !variants.has(variant))) {
      throw new Error(`invalid result: ${JSON.stringify({ path, variant, status })}`);
    }
    paths.add(path);
    if (!files.has(path)) files.set(path, new Map());
    const rows = files.get(path);
    if (rows.has(variant)) throw new Error(`duplicate result for ${path} (${variant})`);
    if (rows.size && (variant === null || rows.has(null))) {
      throw new Error(`invalid metadata and executable variants for the same file: ${path}`);
    }
    rows.set(variant, status);
  }
  if (paths.size !== report.selection?.testedFiles) {
    throw new Error('distinct file count differs from selection.testedFiles');
  }
  const { paths: filters, offset, limit, availableFiles } = report.selection;
  if (!Array.isArray(filters) || !filters.length || filters.some(path => typeof path !== 'string') ||
      !Number.isSafeInteger(availableFiles) || availableFiles < 1 ||
      !Number.isSafeInteger(offset) || offset < 0 ||
      (limit !== null && (!Number.isSafeInteger(limit) || limit < 1)) ||
      paths.size !== Math.min(limit ?? availableFiles, Math.max(0, availableFiles - offset))) {
    throw new Error('invalid runner selection or selected file count');
  }
  return filters.length === 1 && filters[0] === '.' ? availableFiles : null;
}

function counts() {
  return { testedFiles: 0, passedFiles: 0, totalVariants: 0,
    statuses: Object.fromEntries(statuses.map(status => [status, 0])) };
}

async function summarize(inputs, output) {
  const files = new Map();
  const suiteSizes = new Set();
  let scope;
  for (const input of inputs) {
    try {
      const data = await readFile(input);
      const report = JSON.parse((input.endsWith('.gz') ? gunzipSync(data) : data).toString('utf8'));
      const current = identity(report);
      scope ??= current;
      if (JSON.stringify(current) !== JSON.stringify(scope)) {
        throw new Error('Test262 revision, runner hash, worker hash or execution budgets differ');
      }
      suiteSizes.add(collect(files, report));
    } catch (error) {
      throw new Error(`${input}: ${error.message}`);
    }
  }
  const overall = counts();
  const folders = new Map();
  const lines = [];
  for (const path of [...files.keys()].sort()) {
    const rows = files.get(path);
    for (const variant of [...rows.keys()].sort()) {
      lines.push(`${path}\t${variant ?? ''}\t${rows.get(variant)}\n`);
    }
    const folder = path.slice(5).includes('/') ? path.split('/')[1] : '(root)';
    if (!folders.has(folder)) folders.set(folder, counts());
    for (const bucket of [overall, folders.get(folder)]) {
      bucket.testedFiles++;
      if ([...rows.values()].every(status => status === 'pass')) bucket.passedFiles++;
      bucket.totalVariants += rows.size;
      for (const status of rows.values()) bucket.statuses[status]++;
    }
  }
  const fullSuite = suiteSizes.size === 1 && suiteSizes.has(files.size);
  const data = Buffer.from(lines.join(''), 'utf8');
  const outcomesSha256 = createHash('sha256').update(data).digest('hex');
  if (output) await writeFile(output, gzipSync(data, { level: 9 }));
  return { ...scope, reportCount: inputs.length, fullSuite, outcomesSha256, overall, byFolder: Object.fromEntries(folders) };
}

try {
  const inputs = process.argv.slice(2);
  const flag = inputs.indexOf('--outcomes');
  let output;
  if (flag !== -1) {
    [, output] = inputs.splice(flag, 2);
    if (!output?.endsWith('.tsv.gz')) throw new Error('--outcomes requires a .tsv.gz filename');
  }
  if (!inputs.length || inputs.some(input => input.startsWith('--'))) {
    throw new Error('Usage: node tools/test262/summarize.mjs [--outcomes PATH.tsv.gz] REPORT.json [REPORT.json.gz ...]');
  }
  const summary = await summarize(inputs, output);
  process.stdout.write(`${JSON.stringify(summary, null, 2)}\n`);
} catch (error) {
  process.stderr.write(`${error.message}\n`);
  process.exitCode = 2;
}
