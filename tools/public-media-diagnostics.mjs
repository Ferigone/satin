// Export known classes only; never retain arbitrary errors, URLs, or account data.
const kinds = /\b(?:ReferenceError|TypeError|SyntaxError|UnsupportedFeature|ExecutionLimit|HeapLimit|RangeError|TimeoutError|NetworkError|SecurityError|NotSupportedError)\b/;
const causes = [
  'Expression nesting limit exceeded', 'Regular expression nesting limit exceeded',
  'Aggregate allocation byte budget exhausted', 'Script compilation memory budget exhausted',
  'Heap cell budget exhausted', 'Binary object budget exhausted',
  'Instruction budget exhausted', 'Native work budget exhausted',
  'Nested bytecode depth budget exhausted', 'Call depth budget exhausted',
  'Microtask checkpoint budget exceeded', 'Dynamic source byte budget exhausted',
  'String code-unit budget exhausted', 'CPU time budget exhausted',
  'Value is not callable', 'Property is not an object', 'Value is not an object',
  'Source budget exceeded', 'is not defined', 'connection closed', 'timed out',
  'Unhandled promise rejection', 'Failed to load resource', 'integrity mismatch',
  'Unsupported Media Source type', 'The operation is not supported',
];
const phases = [
  ['Dynamic import:', 'dynamic-import'], ['Module:', 'module'],
  ['Dynamic inline script:', 'dynamic-inline'], ['Written inline script:', 'document-write'],
  ['Unhandled promise rejection:', 'promise'], ['Worker:', 'worker'],
  ['Media Source:', 'media-source'], ['Media element:', 'media-element'],
  ['SourceBuffer:', 'source-buffer'], ['IntersectionObserver layout:', 'observer-layout'],
];
const origins = [
  ['twitch-cdn', 'assets.twitch.tv'], ['twitch', 'www.twitch.tv'],
  ['yt-static', 'www.gstatic.com'], ['youtube', 'www.youtube.com'],
  ['yt-assets', 'i.ytimg.com'],
];
const scriptAssets = [
  ['twitch-core', /^https:\/\/assets\.twitch\.tv\/assets\/core-[a-z0-9.-]+\.js(?:[:?\s]|$)/i],
  ['youtube-player', /^https:\/\/www\.youtube\.com\/s\/player\/[a-z0-9_./-]+\.js(?:[:?\s]|$)/i],
  ['youtube-desktop', /^https:\/\/www\.youtube\.com\/s\/desktop\/[a-z0-9_./-]+\.js(?:[:?\s]|$)/i],
];

export function safeDiagnostic(line) {
  const text = String(line);
  const lower = text.toLowerCase();
  const inline = text.match(/\binline script #(\d{1,7})\b/i)?.[1] ?? null;
  const phase = phases.find(([prefix]) => text.startsWith(prefix))?.[1]
    ?? (inline ? 'inline-script' : null);
  const host = text.match(/^https?:\/\/([a-z0-9.-]+)(?=[:/ ?#]|$)/i)?.[1]?.toLowerCase();
  const source = host ? (origins.find(([, domain]) => host === domain)?.[0] ?? 'other') : null;
  return {
    kind: text.match(kinds)?.[0] ?? 'Other',
    cause: causes.find(cause => lower.includes(cause.toLowerCase())) ?? null,
    inline, phase, source,
    asset: scriptAssets.find(([, match]) => match.test(text))?.[0] ?? null,
  };
}

// Fixed diagnostic fields only. Never spread a parsed line into a public receipt.
export function executionProfiles(stderr) {
  const labels = ['other', 'entryCompile', 'regexpCompile', 'regexpMatch', 'propertyRead', 'host'];
  const number = n => Number.isSafeInteger(n) && n >= 0;
  const vector = v => Array.isArray(v) && v.length === labels.length && v.every(number);
  const named = v => Object.fromEntries(labels.map((label, i) => [label, v[i]]));
  const results = [];
  for (const line of String(stderr).split(/\r?\n/)) {
    if (!line.startsWith('SATIN_QA_SCRIPT ') || line.length > 1024) continue;
    let p;
    try { p = JSON.parse(line.slice('SATIN_QA_SCRIPT '.length)); } catch { continue; }
    if (!p || !number(p.node) || !number(p.bytes) || typeof p.external !== 'boolean'
      || typeof p.sha256 !== 'string' || !/^[a-f0-9]{64}$/.test(p.sha256)
      || !['ExecutionLimit', 'HeapLimit', 'SyntaxError', 'TypeError', 'ReferenceError',
        'RangeError', 'UnsupportedFeature', 'InternalError', 'Thrown', 'Error', 'EvalError',
        'UriError', 'AggregateError', 'CompileError', 'LinkError', 'RuntimeError'].includes(p.error)
      || !number(p.wallUs) || !(p.cpuUs === null || number(p.cpuUs))
      || !vector(p.work) || !(p.cycles === null || vector(p.cycles))) continue;
    results.push({ node: p.node, external: p.external, sha256: p.sha256, bytes: p.bytes,
      error: p.error, wallUs: p.wallUs, cpuUs: p.cpuUs, work: named(p.work),
      cycles: p.cycles && named(p.cycles) });
    if (results.length === 128) break;
  }
  return results;
}

export function budgetTrace(stderr) {
  const sourcePath = raw => /(?:^|\/)(crates\/(?:engine|javascript)\/src\/[A-Za-z0-9_./-]+\.rs)$/
    .exec(raw.replaceAll('\\', '/'))?.[1] ?? null;
  const records = [];
  for (const line of String(stderr).split(/\r?\n/)) {
    const cost = /^SATIN_QA_COST (\d{1,15}) (\d{1,6}) (.+)$/.exec(line);
    const parent = records.at(-1);
    const compiler = /^SATIN_QA_COMPILER (\d{1,15}) (\d{1,15}) (\d{1,15}) (\d{1,15})(?: (\d{1,15}))?$/.exec(line);
    if (compiler && parent?.kind === 'compiler' && !parent.compilation) {
      const [, initialBytes, sourceUnits, backingBytes, offset] = compiler.map(Number);
      parent.compilation = { initialBytes, sourceUnits, backingBytes, offset };
      if (compiler[5] !== undefined) parent.compilation.compactedBodies = Number(compiler[5]);
      continue;
    }
    if (cost && ['instruction', 'native'].includes(parent?.kind)) {
      const costs = parent.topWork ??= [];
      if (costs.length < 8) costs.push({ units: +cost[1], line: +cost[2], file: sourcePath(cost[3]) });
      continue;
    }
    const record = /^SATIN_QA_BUDGET (heap|instruction|native|compiler) (\d{1,15}) (\d{1,15}) (\d{1,15}) (\d{1,6}) (.+)$/.exec(line);
    if (!record) continue;
    if (records.length === 128) break;
    const [, kind, requested, remaining, nativeWork, lineNumber, rawPath] = record;
    records.push({ kind, requested: +requested, remaining: +remaining,
      nativeWork: +nativeWork, line: +lineNumber, file: sourcePath(rawPath) });
  }
  return records;
}

// A site is measured in code points. The tag matches eight points from each
// end of the function. Neither source text nor callable names leave the VM.
export function functionProfiles(stderr) {
  const uint = value => Number.isSafeInteger(value) && value >= 0;
  const site = r => r.slice(0, 4).every(uint) && r[1] <= r[2] && r[2] <= r[0] && r[3] <= 0xffffffff;
  const fixed = (r, n) => Array.isArray(r) && r.length === n && site(r);
  const records = [];
  for (const line of String(stderr).split(/\r?\n/)) {
    if (!line.startsWith('SATIN_QA_FUNCTIONS ') || line.length > 8192) continue;
    let p;
    try { p = JSON.parse(line.slice('SATIN_QA_FUNCTIONS '.length)); } catch { continue; }
    if (!p || typeof p.sha256 !== 'string' || !/^[a-f0-9]{64}$/.test(p.sha256)
      || !uint(p.unassignedWork) || !Array.isArray(p.top) || p.top.length > 24
      || !p.top.every(r => fixed(r, 6) && uint(r[4]) && r[4] > 0 && uint(r[5]))
      || !Array.isArray(p.stack) || p.stack.length > 24 || !p.stack.every(r => fixed(r, 4))) continue;
    records.push({ sha256: p.sha256, top: p.top, stack: p.stack, unassignedWork: p.unassignedWork });
    if (records.length >= 128) break;
  }
  return records;
}
