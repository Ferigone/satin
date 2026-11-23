import { createHash } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { basename, dirname, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { completeCounters, count, site } from './website.mjs';

const title = 'ECMAScript Language – test262';
const resultKeys = ['ran', 'pass', 'fail', 'failedToLoad'];
const fields = {
  run: {
    planned: ['id', 'testsToRun'], ran: ['id', 'totalCounter'], pass: ['id', 'Pass'],
    fail: ['id', 'Fail'], failedToLoad: ['id', 'failedToLoadCounter1'],
  },
  results: {
    ran: ['class', 'totalCases'], pass: ['class', 'passedCases'], fail: ['class', 'failedCases'],
    failedToLoad: ['id', 'failedToLoadCounter'],
  },
};
const pngSignature = Buffer.from('89504e470d0a1a0a', 'hex');
const pngEnd = Buffer.from('0000000049454e44ae426082', 'hex');

function requireEvidence(condition, message) {
  if (!condition) throw new Error(message);
}

export function validateNativeHarnessManifest(manifest) {
  requireEvidence(manifest?.schema === 1 && manifest.scope === 'legacy-test262-native-chapter' &&
    manifest.url === site, 'Invalid native harness manifest scope');
  const { selection, suite, files } = manifest;
  requireEvidence(selection?.kind === 'chapter' && selection.name === 'harness' && selection.planned === 55,
    'Manifest must identify the 55-case harness chapter');
  requireEvidence(typeof suite?.version === 'string' && suite.version.trim() &&
    typeof suite.date === 'string' && /^\d{4}-\d{2}-\d{2}$/.test(suite.date), 'Missing manifest suite identity');
  const required = ['process', 'stdout', 'frame', ...['actions', 'selectionFrame'].filter(key => Object.hasOwn(files ?? {}, key))];
  requireEvidence(required.every(key => typeof files?.[key] === 'string' &&
    /^[a-z\d][a-z\d_.-]*$/i.test(files[key]) && basename(files[key]) === files[key]),
  'Manifest files must be simple filenames beside the manifest');
  const names = [...required.map(key => files[key]), `${files.frame}.run.json`, `${files.frame}.results.json`];
  requireEvidence(new Set(names.map(name => name.toLowerCase())).size === names.length && /\.png$/i.test(files.frame) &&
    (!files.selectionFrame || /\.png$/i.test(files.selectionFrame)), 'Manifest files must be distinct, with PNG frames');
  return manifest;
}

const sameSite = url => typeof url === 'string' && url.split('#')[0] === site;
const hasClass = (attrs, name) => attrs.class?.split(/\s+/).includes(name);
const hidden = attrs => Object.hasOwn(attrs, 'hidden') || /(?:^|;)\s*display\s*:\s*none\s*(?:!important)?\s*(?:;|$)/i.test(attrs.style ?? '');

// Only read the native serializer's fixed Test262 elements, not arbitrary DOM.
// Ignore comments and raw script/style text. Quoted attributes may contain
// spaces and delimiter characters; do not reinterpret their content as tags.
const tagAttributes = String.raw`(?:"[^"]*"|'[^']*'|[^'"<>])*`;
const withoutInert = html => html.replace(
  new RegExp(`<!--[\\s\\S]*?-->|<(script|style|template|textarea)\\b${tagAttributes}>[\\s\\S]*?<\\/\\1\\s*>`, 'gi'), '',
);

function regions(html, tag, matches, label) {
  const tags = [...html.matchAll(new RegExp(`<(/?)${tag}(?=[\\s>])(${tagAttributes})>`, 'gi'))];
  const attributes = raw => Object.fromEntries(
    Array.from(raw.matchAll(/(?:^|\s)([\w:-]+)\s*=\s*(?:"([^"]*)"|'([^']*)')/g),
      ([, key, double, single]) => [key.toLowerCase(), double ?? single]),
  );
  const candidates = tags.flatMap((match, i) => {
    const attrs = attributes(match[2]);
    return !match[1] && matches(attrs) ? [{ attrs, i }] : [];
  });
  return candidates.map(({ attrs, i }) => {
    let depth = 1;
    for (let j = i + 1; j < tags.length; j++) {
      depth += tags[j][1] ? -1 : 1;
      if (depth === 0) return { attrs, body: html.slice(tags[i].index + tags[i][0].length, tags[j].index) };
    }
    throw new Error(`Unclosed ${label}`);
  });
}

function region(html, tag, matches, label) {
  const candidates = regions(html, tag, matches, label);
  requireEvidence(candidates.length === 1, `Missing or ambiguous ${label}`);
  return candidates[0];
}

const byId = (html, tag, id) => region(html, tag, attrs => attrs.id === id, `#${id}`);
const byClass = (html, tag, name) => region(html, tag, attrs => hasClass(attrs, name), `.${name}`);

function text(element) {
  requireEvidence(!hidden(element.attrs) && !element.body.includes('<'), 'Expected visible plain text');
  return element.body.trim();
}

function selectedPane(html, nav, pane) {
  const tab = byId(html, 'a', nav);
  const content = byClass(html, 'div', pane);
  requireEvidence(hasClass(tab.attrs, 'selected') && !hidden(tab.attrs) && !hidden(content.attrs), `${nav} pane not selected`);
  return content.body;
}

function validPng(png) {
  return Buffer.isBuffer(png) && png.length >= 45 && png.subarray(0, 8).equals(pngSignature) &&
    png.readUInt32BE(8) === 13 && png.toString('ascii', 12, 16) === 'IHDR' &&
    png.readUInt32BE(16) > 0 && png.readUInt32BE(20) > 0 && png.subarray(-12).equals(pngEnd);
}

function observeCounters(snapshot, stage, artifactState) {
  const selectors = fields[stage];
  const all = state => Object.fromEntries(Object.keys(selectors).map(key => [key, { state, raw: null, value: null }]));
  if (snapshot == null) return all(artifactState === 'invalid' ? 'invalid-snapshot' : 'missing-snapshot');
  if (typeof snapshot.html !== 'string' || !snapshot.html) return all('invalid-snapshot');
  const html = withoutInert(snapshot.html);
  const paneClass = stage === 'run' ? 'content-tests' : 'content-results';
  try {
    const panes = regions(html, 'div', attrs => hasClass(attrs, paneClass), `.${paneClass}`);
    if (panes.length !== 1) return all(panes.length ? 'ambiguous-pane' : 'missing-pane');
    return Object.fromEntries(Object.entries(selectors).map(([key, [where, name]]) => {
      const matches = regions(panes[0].body, 'span', attrs => where === 'id' ? attrs.id === name : hasClass(attrs, name), name);
      if (matches.length !== 1) return [key, { state: matches.length ? 'ambiguous-element' : 'missing-element', raw: null, value: null }];
      const raw = matches[0].body.trim();
      const value = count(raw);
      const state = value !== null ? 'observed' : raw === '' ? 'blank' : 'invalid-value';
      return [key, { state, raw, value }];
    }));
  } catch {
    return all('invalid-snapshot');
  }
}

export function verifyNativeHarnessArtifacts({ manifest, process, summary, run, results, png }) {
  const { selection, suite: expectedSuite } = validateNativeHarnessManifest(manifest);
  const { planned } = selection;
  requireEvidence(process?.code === 0 && !process.signal && !process.timedOut && !process.spawnError, 'Native process did not succeed');
  requireEvidence(summary?.window_smoke === true && Number.isSafeInteger(summary.frames_presented) &&
    summary.frames_presented > 0 && summary.layout_limited === false && summary.paint_limited === false &&
    sameSite(summary.url) && summary.title === title, 'Missing successful native frame summary');
  const envelope = summary.checkpoints;
  requireEvidence(typeof envelope?.runId === 'string' && /^\d+-\d+$/.test(envelope.runId) &&
    Array.isArray(envelope.names) && envelope.names.length === 2 &&
    envelope.names[0] === 'run' && envelope.names[1] === 'results', 'Missing ordered checkpoint success envelope');
  requireEvidence(validPng(png), 'Missing or invalid final PNG');
  for (const snapshot of [run, results]) {
    requireEvidence(snapshot?.runId === envelope.runId, 'Checkpoint is stale or from another attempt');
    requireEvidence(sameSite(snapshot.url) && snapshot.title === title &&
      Number.isSafeInteger(snapshot.tabId) && snapshot.tabId > 0 &&
      Number.isSafeInteger(snapshot.documentNavigation) && snapshot.documentNavigation > 0 &&
      typeof snapshot.html === 'string' && snapshot.html.length > 0, 'Missing or invalid checkpoint document');
  }
  requireEvidence(run.tabId === results.tabId && run.documentNavigation === results.documentNavigation, 'Tab or document changed');

  const runPane = selectedPane(withoutInert(run.html), 'run', 'content-tests');
  const resultsPane = selectedPane(withoutInert(results.html), 'results', 'content-results');
  const progress = byId(runPane, 'div', 'progressbar');
  requireEvidence(!hidden(progress.attrs) && text(byClass(progress.body, 'span', 'text')) === 'Testing complete!',
    'Run completion marker absent');
  const chapters = byId(runPane, 'div', 'chapterSelector').body;
  const sameSize = regions(chapters, 'td', attrs => hasClass(attrs, 'chapterName'), 'chapter label')
    .map(text).filter(label => label.endsWith(`(${planned} tests)`));
  requireEvidence(sameSize.length === 1 && sameSize[0] === `Chapter - ${selection.name} (${planned} tests)`, 'Wrong or ambiguous chapter');

  const counters = (stage, pane) => Object.fromEntries(Object.entries(fields[stage]).map(([key, [where, name]]) =>
    [key, text((where === 'id' ? byId : byClass)(pane, 'span', name))]));
  const runCounters = counters('run', runPane);
  requireEvidence(completeCounters(runCounters, planned), 'Run counters do not establish all 55 harness tests');
  const resultsCounters = counters('results', resultsPane);
  requireEvidence(resultKeys.every(key => count(resultsCounters[key]) === count(runCounters[key])), 'Results counters do not match Run');
  const suite = pane => ({
    version: text(byClass(pane, 'span', 'targetTestSuiteVersion')),
    date: text(byClass(pane, 'span', 'targetTestSuiteDate')),
  });
  const runSuite = suite(runPane), resultsSuite = suite(resultsPane);
  requireEvidence([runSuite, resultsSuite].every(observed =>
    observed.version === expectedSuite.version && observed.date === expectedSuite.date), 'Suite identity is missing or changed');
  return {
    status: 'artifacts-consistent',
    visualConfirmationRequired: true,
    selection: { kind: selection.kind, name: selection.name, planned },
    suite: runSuite,
    counters: Object.fromEntries(Object.entries(runCounters).map(([key, value]) => [key, count(value)])),
  };
}

export function reportNativeHarnessArtifacts(input) {
  const { selection } = validateNativeHarnessManifest(input.manifest);
  const observed = Object.fromEntries(['run', 'results'].map(stage =>
    [stage, observeCounters(input[stage], stage, input.artifacts?.[stage])]));
  try {
    return { ...verifyNativeHarnessArtifacts(input), observed };
  } catch (error) {
    const countSeen = observed.run.ran.value;
    const partial = observed.run.planned.value === selection.planned && countSeen > 0 && countSeen < selection.planned;
    return {
      status: input.process?.timedOut ? 'timeout' : partial ? 'partial' : 'unverified',
      selection, observed, error: error.message,
    };
  }
}

function verifyChapterActions(actions) {
  requireEvidence(Array.isArray(actions), 'Missing chapter action record');
  const steps = actions[0]?.action === 'title' && actions[0].value === title ? actions.slice(1) : actions;
  const [nav, row, run, resultsNav, results] = steps;
  const checkpoint = (action, name, root, className, text) => action?.action === 'checkpoint' &&
    action.name === name && action.within_id === root && action.descendant_class === className &&
    action.text === text && Number.isSafeInteger(action.timeout_ms) && action.timeout_ms > 0 && action.timeout_ms <= 15_000;
  requireEvidence(steps.length === 5 && nav?.action === 'click' && nav.id === 'run' &&
    row?.action === 'click_at' && [row.x, row.y].every(value => Number.isFinite(value) && value >= 0) &&
    checkpoint(run, 'run', 'progressbar', 'text', 'Testing complete!') &&
    resultsNav?.action === 'click' && resultsNav.id === 'results' &&
    checkpoint(results, 'results', 'contentContainer', 'totalCases', '55'), 'Unexpected chapter or Results action sequence');
  return row;
}

function verifyChapterVisualReview(input, counters, row) {
  const review = input.manifest.visualReview;
  requireEvidence(typeof review?.reviewer === 'string' && review.reviewer.trim() &&
    review.runId === input.summary.checkpoints.runId, 'Missing visual review for this attempt');
  for (const key of ['actions', 'selectionPng', 'png', 'run', 'results']) {
    requireEvidence(/^[a-f0-9]{64}$/.test(review.sha256?.[key] ?? '') &&
      review.sha256[key] === input.digests?.[key], `Visual review does not match ${key} artifact`);
  }
  const selection = review.selection;
  requireEvidence(validPng(input.selectionPng) &&
    input.selectionPng.readUInt32BE(16) === input.png.readUInt32BE(16) &&
    input.selectionPng.readUInt32BE(20) === input.png.readUInt32BE(20) &&
    selection?.label === 'Chapter - harness (55 tests)' && selection.control === 'Run' &&
    selection.x === row.x && selection.y === row.y && Number.isFinite(selection.scale) && selection.scale > 0 &&
    row.x * selection.scale < input.png.readUInt32BE(16) && row.y * selection.scale < input.png.readUInt32BE(20),
  'Visual review did not confirm the chapter Run hit');
  requireEvidence(review.results?.current === true && resultKeys.every(key => review.results.counters?.[key] === counters[key]),
    'Visual review did not confirm current matching Results');
}

// This intake never starts Satin. It promotes an already-captured workflow
// only with both machine-checked evidence and the coordinator's visual review.
export function assessNativeHarnessCapture(input) {
  const nativeEvidence = reportNativeHarnessArtifacts(input);
  const entry = {
    websiteRun: 'unverified', selection: nativeEvidence.selection, nativeEvidence,
    artifactStates: input.artifacts, firstFailedStage: null,
  };
  if (nativeEvidence.status !== 'artifacts-consistent') {
    entry.firstFailedStage = nativeEvidence.status === 'timeout' ? 'native-timeout' :
      nativeEvidence.status === 'partial' ? 'chapter-completion' : 'native-artifacts';
    entry.error = nativeEvidence.error;
    return entry;
  }
  let row;
  try {
    row = verifyChapterActions(input.actions);
  } catch (error) {
    return { ...entry, firstFailedStage: 'chapter-selection', error: error.message };
  }
  try {
    verifyChapterVisualReview(input, nativeEvidence.counters, row);
  } catch (error) {
    return { ...entry, firstFailedStage: 'visual-review', error: error.message };
  }
  return {
    ...entry, websiteRun: 'complete',
    visualReview: { reviewer: input.manifest.visualReview.reviewer, runId: input.manifest.visualReview.runId },
    snapshots: { complete: { counters: nativeEvidence.counters } },
  };
}

const parseJson = bytes => JSON.parse(bytes.toString('utf8').replace(/^\uFEFF/, ''));

export async function readNativeHarnessArtifacts(manifestPath) {
  const manifest = validateNativeHarnessManifest(parseJson(await readFile(manifestPath)));
  const { files } = manifest;
  const entries = {
    process: files.process, summary: files.stdout, png: files.frame,
    run: `${files.frame}.run.json`, results: `${files.frame}.results.json`,
    ...(files.actions ? { actions: files.actions } : {}),
    ...(files.selectionFrame ? { selectionPng: files.selectionFrame } : {}),
  };
  const dir = dirname(resolve(manifestPath));
  const capture = { manifest, artifacts: {}, digests: {} };
  await Promise.all(Object.entries(entries).map(async ([key, file]) => {
    try {
      const bytes = await readFile(resolve(dir, file));
      capture[key] = key === 'png' || key === 'selectionPng' ? bytes : parseJson(bytes);
      capture.digests[key] = createHash('sha256').update(bytes).digest('hex');
      capture.artifacts[key] = capture[key] == null ? 'invalid' : 'present';
    } catch (error) {
      if (error.code !== 'ENOENT' && !(error instanceof SyntaxError)) throw error;
      capture[key] = null;
      capture.artifacts[key] = error.code === 'ENOENT' ? 'missing' : 'invalid';
    }
  }));
  return capture;
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  try {
    requireEvidence(process.argv.length === 3, 'Usage: node native-checkpoint-verify.mjs MANIFEST_JSON');
    const report = reportNativeHarnessArtifacts(await readNativeHarnessArtifacts(process.argv[2]));
    console.log(JSON.stringify(report));
    if (report.status !== 'artifacts-consistent') process.exitCode = 1;
  } catch (error) {
    console.error(String(error));
    process.exitCode = 1;
  }
}
