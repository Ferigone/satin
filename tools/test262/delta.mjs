import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

const variants = new Set(['sloppy', 'strict', 'module', 'raw', null]);
const statuses = new Set([
  'pass', 'fail', 'unsupported', 'resource_limit', 'async_incomplete',
  'harness_error', 'timeout', 'crash', 'invalid_metadata', 'excluded',
]);

function scope(report, label) {
  if (report?.schema !== 1) throw new Error(`${label}: expected report schema 1`);
  if (typeof report.test262Revision !== 'string' || !/^[0-9a-f]{40}$/.test(report.test262Revision)) {
    throw new Error(`${label}: expected a full Test262 revision`);
  }
  const { paths, offset, limit } = report.selection ?? {};
  if (!Array.isArray(paths) || !paths.length || paths.some(path => typeof path !== 'string') ||
      !Number.isSafeInteger(offset) || offset < 0 ||
      (limit !== null && (!Number.isSafeInteger(limit) || limit < 1))) {
    throw new Error(`${label}: invalid selection (expected paths, offset and limit)`);
  }
  // The runner unions path selections before sorting and applying offset/limit.
  return { test262Revision: report.test262Revision, paths: [...new Set(paths)].sort(), offset, limit };
}

function indexResults(report, label) {
  if (!Array.isArray(report.results)) throw new Error(`${label}: expected a results array`);
  const indexed = new Map();
  for (const [index, result] of report.results.entries()) {
    if (typeof result?.path !== 'string' || !result.path.startsWith('test/') || !result.path.endsWith('.js') ||
        !variants.has(result.variant) || !statuses.has(result.status)) {
      throw new Error(`${label}: invalid result at index ${index} (expected path, variant and status)`);
    }
    const key = JSON.stringify([result.path, result.variant]);
    if (indexed.has(key)) throw new Error(`${label}: duplicate result for ${key}`);
    indexed.set(key, result);
  }
  return indexed;
}

function limits(report, label) {
  if (!Object.hasOwn(report, 'limits')) return null; // legacy schema-1 report
  const { timeoutMs, maxInstructions } = report.limits ?? {};
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 2_147_483_647 ||
      !Number.isSafeInteger(maxInstructions) || maxInstructions < 1 || maxInstructions > 100_000_000) {
    throw new Error(`${label}: invalid limits (expected timeoutMs and maxInstructions)`);
  }
  return { timeoutMs, maxInstructions };
}

export function compareReports(baseline, candidate) {
  const baselineScope = scope(baseline, 'baseline');
  const candidateScope = scope(candidate, 'candidate');
  const mismatches = Object.keys(baselineScope).flatMap(field => {
    const before = JSON.stringify(baselineScope[field]);
    const after = JSON.stringify(candidateScope[field]);
    return before === after ? [] : [`${field}: baseline ${before}, candidate ${after}`];
  });
  const baselineLimits = limits(baseline, 'baseline');
  const candidateLimits = limits(candidate, 'candidate');
  const limitsVerified = baselineLimits !== null && candidateLimits !== null;
  if (limitsVerified) {
    for (const field of Object.keys(baselineLimits)) {
      if (baselineLimits[field] !== candidateLimits[field]) {
        mismatches.push(`${field}: baseline ${baselineLimits[field]}, candidate ${candidateLimits[field]}`);
      }
    }
  }
  if (mismatches.length) throw new Error(`Test262 scope mismatch:\n${mismatches.join('\n')}`);

  const before = indexResults(baseline, 'baseline');
  const after = indexResults(candidate, 'candidate');
  const changes = { regressions: [], improvements: [], nonPassingChanges: [], new: [], missing: [], excluded: [] };
  let unchanged = 0;
  let lostCoverage = 0;
  for (const key of [...new Set([...before.keys(), ...after.keys()])].sort()) {
    const previous = before.get(key);
    const current = after.get(key);
    const baselineStatus = previous?.status ?? null;
    const candidateStatus = current?.status ?? null;
    if (baselineStatus === candidateStatus) {
      unchanged++;
      continue;
    }

    const { path, variant } = previous ?? current;
    const entry = { path, variant, baselineStatus, candidateStatus };
    if (previous && baselineStatus !== 'excluded' && (!current || candidateStatus === 'excluded')) lostCoverage++;
    if (baselineStatus === 'excluded' || candidateStatus === 'excluded') changes.excluded.push(entry);
    else if (!previous) changes.new.push(entry);
    else if (!current) changes.missing.push(entry);
    else if (baselineStatus === 'pass') changes.regressions.push(entry);
    else if (candidateStatus === 'pass') changes.improvements.push(entry);
    else changes.nonPassingChanges.push(entry);
  }

  return {
    schema: 1,
    scope: baselineScope,
    limits: { baseline: baselineLimits, candidate: candidateLimits, verified: limitsVerified },
    workerSha256: { baseline: baseline.workerSha256 ?? null, candidate: candidate.workerSha256 ?? null },
    runnerSha256: { baseline: baseline.runnerSha256 ?? null, candidate: candidate.runnerSha256 ?? null },
    counts: { unchanged, lostCoverage, ...Object.fromEntries(Object.entries(changes).map(([key, rows]) => [key, rows.length])) },
    changes,
  };
}

const usage = 'Usage: node tools/test262/delta.mjs BASELINE.json CANDIDATE.json';

async function readReport(file, label) {
  try {
    return JSON.parse(await readFile(file, 'utf8'));
  } catch (error) {
    throw new Error(`${label} (${file}): ${error.message}`);
  }
}

async function main(args) {
  if (args.length === 1 && ['--help', '-h'].includes(args[0])) {
    process.stdout.write(`${usage}
Exit codes: 0 = no regression or coverage loss, 1 = regression or coverage loss,
             2 = invalid input or scope mismatch.
Limits are compared when present in both input reports.
`);
    return 0;
  }
  if (args.length !== 2) throw new Error(usage);
  const [baseline, candidate] = await Promise.all([
    readReport(args[0], 'baseline'),
    readReport(args[1], 'candidate'),
  ]);
  const delta = compareReports(baseline, candidate);
  process.stdout.write(`${JSON.stringify(delta, null, 2)}\n`);
  return delta.counts.regressions || delta.counts.lostCoverage ? 1 : 0;
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  try {
    process.exitCode = await main(process.argv.slice(2));
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exitCode = 2;
  }
}
