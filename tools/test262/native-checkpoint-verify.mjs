import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { completeCounters, count, site } from './website.mjs';

const title = 'ECMAScript Language – test262';
const planned = 55;
const resultKeys = ['ran', 'pass', 'fail', 'failedToLoad'];
const pngSignature = Buffer.from('89504e470d0a1a0a', 'hex');
const pngEnd = Buffer.from('0000000049454e44ae426082', 'hex');

function requireEvidence(condition, message) {
  if (!condition) throw new Error(message);
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

export function verifyNativeHarnessArtifacts({ process, summary, run, results, png }) {
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
  const fiftyFive = regions(chapters, 'td', attrs => hasClass(attrs, 'chapterName'), 'chapter label')
    .map(text).filter(label => /\(55 tests\)$/.test(label));
  requireEvidence(fiftyFive.length === 1 && fiftyFive[0] === 'Chapter - harness (55 tests)', 'Wrong or ambiguous chapter');

  const ids = { planned: 'testsToRun', ran: 'totalCounter', pass: 'Pass', fail: 'Fail', failedToLoad: 'failedToLoadCounter1' };
  const runCounters = Object.fromEntries(Object.entries(ids).map(([key, id]) => [key, text(byId(runPane, 'span', id))]));
  requireEvidence(completeCounters(runCounters, planned), 'Run counters do not establish all 55 harness tests');
  const resultsCounters = {
    ran: text(byClass(resultsPane, 'span', 'totalCases')),
    pass: text(byClass(resultsPane, 'span', 'passedCases')),
    fail: text(byClass(resultsPane, 'span', 'failedCases')),
    failedToLoad: text(byId(resultsPane, 'span', 'failedToLoadCounter')),
  };
  requireEvidence(resultKeys.every(key => count(resultsCounters[key]) === count(runCounters[key])), 'Results counters do not match Run');
  const suite = pane => ({
    version: text(byClass(pane, 'span', 'targetTestSuiteVersion')),
    date: text(byClass(pane, 'span', 'targetTestSuiteDate')),
  });
  const runSuite = suite(runPane), resultsSuite = suite(resultsPane);
  requireEvidence(runSuite.version && runSuite.date &&
    runSuite.version === resultsSuite.version && runSuite.date === resultsSuite.date, 'Suite identity is missing or changed');
  return {
    status: 'artifacts-consistent',
    visualConfirmationRequired: true,
    selection: { kind: 'chapter', name: 'harness', planned },
    suite: runSuite,
    counters: Object.fromEntries(Object.entries(runCounters).map(([key, value]) => [key, count(value)])),
  };
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  try {
    const [outcome, stdout, frame] = process.argv.slice(2);
    requireEvidence(process.argv.length === 5, 'Usage: node native-checkpoint-verify.mjs PROCESS_JSON STDOUT_JSON FRAME_PNG');
    const json = async path => JSON.parse((await readFile(path, 'utf8')).replace(/^\uFEFF/, ''));
    const [runProcess, summary, run, results, png] = await Promise.all([
      json(outcome), json(stdout), json(`${frame}.run.json`), json(`${frame}.results.json`), readFile(frame),
    ]);
    console.log(JSON.stringify(verifyNativeHarnessArtifacts({ process: runProcess, summary, run, results, png })));
  } catch (error) {
    console.error(String(error));
    process.exitCode = 1;
  }
}
