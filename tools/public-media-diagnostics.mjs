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

export function budgetTrace(stderr) {
  const sourcePath = raw => /(?:^|\/)(crates\/(?:engine|javascript)\/src\/[A-Za-z0-9_./-]+\.rs)$/
    .exec(raw.replaceAll('\\', '/'))?.[1] ?? null;
  const records = [];
  for (const line of String(stderr).split(/\r?\n/)) {
    const cost = /^SATIN_QA_COST (\d{1,15}) (\d{1,6}) (.+)$/.exec(line);
    const parent = records.at(-1);
    const compiler = /^SATIN_QA_COMPILER (\d{1,15}) (\d{1,15}) (\d{1,15}) (\d{1,15})$/.exec(line);
    if (compiler && parent?.kind === 'compiler' && !parent.compilation) {
      const [, initialBytes, sourceUnits, backingBytes, offset] = compiler.map(Number);
      parent.compilation = { initialBytes, sourceUnits, backingBytes, offset };
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
