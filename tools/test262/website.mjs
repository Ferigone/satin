import { execFile, spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { once } from 'node:events';
import { mkdir, mkdtemp, open, readFile, writeFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { pathToFileURL } from 'node:url';
import { promisify } from 'node:util';

export const site = 'https://v8.github.io/test262/website/default.html';
const title = 'ECMAScript Language – test262';
const counterIds = { planned: 'testsToRun', ran: 'totalCounter', pass: 'Pass', fail: 'Fail', failedToLoad: 'failedToLoadCounter1' };
const chromiumArgs = [
  '--headless=new', '--no-first-run', '--no-default-browser-check', '--disable-background-networking',
  '--disable-background-mode', '--remote-debugging-address=127.0.0.1', '--remote-debugging-port=0',
  '--window-size=1280,900',
];

export function count(text) {
  if (typeof text !== 'string' || !/^\d+$/.test(text.trim())) return null;
  const n = Number(text);
  return Number.isSafeInteger(n) ? n : null;
}

export function countersConsistent(counters) {
  const { planned, ran, pass, fail, failedToLoad } = Object.fromEntries(
    Object.entries(counters).map(([key, value]) => [key, count(value)]),
  );
  // Failed-to-load is a subset of Fail, not an additional outcome.
  return [planned, ran, pass, fail, failedToLoad].every(Number.isSafeInteger) &&
    ran <= planned && ran === pass + fail && failedToLoad <= fail;
}

export function findChapter(labels, name) {
  const matches = labels.flatMap(label => {
    const match = /^Chapter - (.+) \((\d+) tests?\)$/.exec(label);
    const planned = count(match?.[2]);
    return match?.[1] === name && planned > 0 ? [{ kind: 'chapter', name, label, planned }] : [];
  });
  if (matches.length > 1) throw new Error(`Ambiguous chapter: ${name}`);
  return matches[0] ?? null;
}

export function completeCounters(counters, planned) {
  return planned > 0 && countersConsistent(counters) && count(counters.planned) === planned && count(counters.ran) === planned;
}

// Satin's CLI gives us serialized HTML but not a DOM query API. Only use
// simple, known elements for these observations; preserve absent vs empty.
export function satinSnapshot(html) {
  const tagById = id => new RegExp(`<([a-z][\\w-]*)\\b(?=[^>]*\\sid="${id}")[^>]*>`, 'i').exec(html);
  const attr = (tag, key) => new RegExp(`\\s${key}="([^"]*)"`, 'i').exec(tag ?? '')?.[1] ?? null;
  const byId = id => {
    const tag = tagById(id);
    if (!tag) return null;
    const after = html.slice(tag.index + tag[0].length);
    return new RegExp(`^([^<]*)</${tag[1]}>`, 'i').exec(after)?.[1].trim() ?? null;
  };
  return {
    runClass: attr(tagById('run')?.[0], 'class'),
    runTarget: attr(tagById('run')?.[0], 'targetDiv'),
    runAllClass: attr(tagById('btnRunAll')?.[0], 'class'),
    counters: Object.fromEntries(Object.entries(counterIds).map(([key, id]) => [key, byId(id)])),
    activity: byId('nextActivity'),
    suiteVersion: /<span class="targetTestSuiteVersion">([^<]*)<\/span>/i.exec(html)?.[1] ?? null,
    suiteDate: /<span class="targetTestSuiteDate">([^<]*)<\/span>/i.exec(html)?.[1] ?? null,
  };
}

export function satinFailure(load, runTab, native) {
  if (!load?.documentLoaded) return 'document-load';
  if (load.dom?.runTarget !== '.content-tests') return 'site-initialization';
  if (!(count(load.dom.counters.planned) > 0)) return 'catalog-load';
  if (!runTab?.dom?.runClass?.split(/\s+/).includes('selected')) return 'run-tab';
  if (native?.attempt?.stderr?.includes('Smoke target #btnRunAll has no rendered box')) return 'run-all-control';
  if (native?.attempt && native.attempt.process?.code !== 0) return 'native-controls';
  // The CLI cannot observe counters after the actual native sequence. An
  // independent by-ID dispatch can diagnose a handler, not establish a score.
  return null;
}

function launch(executable, args, timeoutMs) {
  const child = spawn(executable, args, { windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
  let stdout = '', stderr = '', spawnError = null, timedOut = false;
  child.stdout.setEncoding('utf8').on('data', chunk => { stdout += chunk; });
  child.stderr.setEncoding('utf8').on('data', chunk => { stderr += chunk; });
  child.on('error', error => { spawnError = String(error); });
  const timer = setTimeout(() => { timedOut = true; child.kill(); }, timeoutMs);
  const done = new Promise(done => child.once('close', (code, signal) => {
    clearTimeout(timer);
    done({ code, signal, spawnError, timedOut, stdout, stderr });
  }));
  return { child, done };
}

async function satin(executable, dir, options, entry) {
  async function invoke(label, args) {
    const profile = await mkdtemp(resolve(dir, `${label}-profile-`));
    const command = ['--url', site, '--profile', profile, ...args];
    const { stdout, stderr, ...process } = await launch(executable, command, Math.max(45_000, options.satinRunMs + 30_000)).done;
    await writeFile(resolve(dir, `${label}.stdout.txt`), stdout);
    await writeFile(resolve(dir, `${label}.stderr.txt`), stderr);
    let summary = null;
    try { summary = JSON.parse(stdout); } catch { /* Retain raw stdout. */ }
    return { args: command, process, stderr, summary };
  }
  async function headless(label, id) {
    const domPath = resolve(dir, `${label}.html`);
    const result = await invoke(label, [
      '--headless', ...(id ? ['--click-id', id] : []), '--run-for-ms', String(options.satinRunMs),
      '--dump-dom', domPath, '--screenshot', resolve(dir, `${label}.png`),
    ]);
    const html = await readFile(domPath, 'utf8').catch(error => {
      if (error.code !== 'ENOENT') throw error;
      return null;
    });
    result.dom = html === null ? null : satinSnapshot(html);
    result.documentLoaded = result.summary?.title === title && sameDocument(result.summary.url);
    return result;
  }

  entry.load = await headless('load');
  entry.runTab = await headless('run-tab-by-id', 'run');
  entry.runAllById = await headless('run-all-by-id-diagnostic', 'btnRunAll');
  if (options.native) {
    const actions = resolve(dir, 'native-actions.json');
    const steps = [{ action: 'title', value: title }, { action: 'click', id: 'run' }];
    const saveActions = (path, steps) => writeFile(path, JSON.stringify(steps, null, 2) + '\n');
    const tabActions = resolve(dir, 'native-tab-actions.json');
    await saveActions(tabActions, steps);
    await saveActions(actions, [...steps, { action: 'click', id: 'btnRunAll' }]);
    entry.native = {};
    for (const [key, label, path] of [['runTab', 'native-run-tab', tabActions], ['attempt', 'native-controls', actions]]) {
      entry.native[key] = await invoke(label, [
        '--smoke-window', resolve(dir, `${label}.png`), '--smoke-actions', path,
      ]);
    }
  }
  entry.firstFailedStage = satinFailure(entry.load, entry.runTab, entry.native);
  entry.firstDiagnostic = entry.load.summary?.javascript?.diagnostics?.[0] ?? null;
  entry.firstResultByIdObserved = entry.runAllById.documentLoaded && count(entry.runAllById.dom?.counters.ran) > 0;
  entry.websiteRun = 'unverified'; // Native smoke gives no post-sequence DOM checkpoint.
}

function cdp(socket, observe) {
  let id = 0;
  const pending = new Map();
  socket.addEventListener('message', event => {
    const message = JSON.parse(event.data);
    if (!message.id) return observe(message);
    pending.get(message.id)?.(message);
  });
  socket.addEventListener('close', () => {
    for (const finish of pending.values()) finish({ error: { message: 'DevTools socket closed' } });
  });
  return (method, params, sessionId) => new Promise((resolve, reject) => {
    const commandId = ++id;
    const finish = message => {
      pending.delete(commandId);
      clearTimeout(timer);
      if (message.error) reject(new Error(`${method}: ${message.error.message}`));
      else resolve(message.result);
    };
    const timer = setTimeout(() => finish({ error: { message: 'command timed out' } }), 5000);
    pending.set(commandId, finish);
    try { socket.send(JSON.stringify({ id: commandId, method, params, sessionId })); }
    catch (error) { finish({ error }); }
  });
}

function sameDocument(url) {
  return typeof url === 'string' && url.split('#')[0] === site;
}

// Executed in a CDP isolated world. This reads UI state only.
function observePage(ids) {
  const node = selector => document.querySelector(selector);
  const text = selector => node(selector)?.textContent.trim() ?? null;
  const visible = selector => node(selector)?.getClientRects().length > 0;
  return {
    url: location.href, title: document.title, readyState: document.readyState,
    homeVisible: visible('.content-home'), runVisible: visible('.content-tests'),
    resultsVisible: visible('.content-results'), runSelected: node('#run.selected') !== null,
    runTarget: node('#run')?.getAttribute('targetDiv') ?? null,
    controls: Object.fromEntries(['btnRunAll', 'btnPause', 'btnResume', 'btnReset'].map(id => [id, visible(`#${id}`)])),
    counters: Object.fromEntries(Object.entries(ids).map(([key, id]) => [key, text(`#${id}`)])),
    activity: text('#nextActivity'), progress: text('#progressbar .text'),
    suiteVersion: text('.targetTestSuiteVersion'), suiteDate: text('.targetTestSuiteDate'),
    chapterRows: document.querySelectorAll('#chapterSelector tr').length,
    loadedChapters: document.querySelectorAll('#chapterSelector tr:not(.waiting):not(.loading)').length,
    chapterLabels: Array.from(document.querySelectorAll('#chapterSelector tr:not(.waiting):not(.loading) .chapterName'),
      cell => cell.textContent.trim()),
    results: {
      ran: text('.totalCases'), pass: text('.passedCases'), fail: text('.failedCases'), failedToLoad: text('#failedToLoadCounter'),
    },
  };
}

// Locate and hit-test; Input.dispatchMouseEvent sends browser pointer input,
// not a synthetic DOM click or a direct call to the site's controller.
function clickPoint(selector, chapterLabel) {
  const matches = Array.from(document.querySelectorAll(selector)).filter(node => chapterLabel === undefined ||
    node.closest('tr')?.querySelector('.chapterName')?.textContent.trim() === chapterLabel);
  const target = matches.length === 1 ? matches[0] : null;
  if (!target?.getClientRects().length) return null;
  target.scrollIntoView({ block: 'nearest', inline: 'nearest' });
  const rect = target.getBoundingClientRect();
  const left = Math.max(0, rect.left), right = Math.min(innerWidth, rect.right);
  const top = Math.max(0, rect.top), bottom = Math.min(innerHeight, rect.bottom);
  if (right <= left || bottom <= top) return null;
  const x = (left + right) / 2, y = (top + bottom) / 2;
  if (!target.contains(document.elementFromPoint(x, y))) return null;
  return { x, y };
}

async function waitFor(probe, predicate, timeoutMs, intervalMs = 100) {
  const deadline = performance.now() + timeoutMs;
  do {
    const value = await probe();
    if (predicate(value)) return value;
    await delay(intervalMs);
  } while (performance.now() < deadline);
  return null;
}

async function chromium(executable, dir, options, entry) {
  const profile = await mkdtemp(resolve(dir, 'chromium-profile-'));
  entry.args = [...chromiumArgs, `--user-data-dir=${profile}`, 'about:blank'];
  // Chromium helpers may inherit stderr on Windows. Redirect to files so
  // waiting for the root process never also waits for their pipe handles.
  const stdout = await open(resolve(dir, 'browser.stdout.txt'), 'w');
  const stderr = await open(resolve(dir, 'browser.stderr.txt'), 'w');
  const child = spawn(executable, entry.args, { windowsHide: true, stdio: ['ignore', stdout.fd, stderr.fd] });
  let spawnError = null;
  child.on('error', error => { spawnError = String(error); });
  const done = new Promise(done => child.once('close', (code, signal) => done({ code, signal, spawnError })));
  await Promise.all([stdout.close(), stderr.close()]);
  const events = entry.events = { diagnostics: [], droppedDiagnostics: 0, network: [] };
  const requests = new Map();
  const started = performance.now();
  let socket, send, sessionId;
  let stage = 'browser-start';
  const elapsed = () => Math.round(performance.now() - started);
  function observe({ method, params }) {
    if (!params) return;
    const { requestId } = params;
    if (method === 'Network.requestWillBeSent' && params.request.url.startsWith(new URL('.', site).href)) {
      const row = { url: params.request.url, type: params.type, atMs: elapsed() };
      events.network.push(row);
      requests.set(requestId, row);
    } else if (method === 'Network.responseReceived' && requests.has(requestId)) {
      const row = requests.get(requestId);
      row.status = params.response.status;
      for (const [name, value] of Object.entries(params.response.headers)) {
        if (['etag', 'last-modified'].includes(name.toLowerCase())) row[name.toLowerCase()] = value;
      }
    } else if (method === 'Network.loadingFailed' && requests.has(requestId)) {
      requests.get(requestId).error = params.errorText;
    }
    const exception = method === 'Runtime.exceptionThrown' && params.exceptionDetails;
    const log = method === 'Log.entryAdded' && ['error', 'warning'].includes(params.entry.level) && params.entry;
    const consoleEntry = method === 'Runtime.consoleAPICalled' && ['error', 'warning'].includes(params.type) && params;
    if (!exception && !log && !consoleEntry) return;
    if (events.diagnostics.length === 1000) { events.droppedDiagnostics++; return; }
    events.diagnostics.push({
      atMs: elapsed(), stage, method, url: (exception || log || consoleEntry)?.url ?? null,
      text: exception ? exception.exception?.description ?? exception.text : log ? log.text :
        consoleEntry.args.map(arg => arg.description ?? arg.value ?? arg.type).join(' '),
    });
  }
  try {
    const endpoint = await waitFor(async () => {
      if (child.exitCode !== null || !child.pid) throw new Error('Chromium exited before opening DevTools');
      const text = await readFile(resolve(profile, 'DevToolsActivePort'), 'utf8').catch(error => {
        if (error.code !== 'ENOENT') throw error;
        return '';
      });
      return /^(\d+)\r?\n(\/devtools\/browser\/[\w-]+)/.exec(text);
    }, Boolean, 12_000);
    if (!endpoint) throw new Error('Chromium did not open DevTools');
    socket = new WebSocket(`ws://127.0.0.1:${endpoint[1]}${endpoint[2]}`);
    await once(socket, 'open', { signal: AbortSignal.timeout(5000) });
    send = cdp(socket, observe);
    entry.version = await send('Browser.getVersion');
    // Own the exact tab instead of depending on browser-specific startup tabs.
    const { targetId } = await send('Target.createTarget', { url: 'about:blank' });
    ({ sessionId } = await send('Target.attachToTarget', { targetId, flatten: true }));
    const cmd = (method, params) => send(method, params, sessionId);
    await Promise.all(['Page', 'Runtime', 'Network', 'Log'].map(domain => cmd(`${domain}.enable`)));

    stage = 'document-load';
    const navigation = await cmd('Page.navigate', { url: site });
    if (navigation.errorText) throw new Error(navigation.errorText);
    async function evaluate(expression, contextId) {
      const result = await cmd('Runtime.evaluate', { expression, contextId, returnByValue: true });
      if (result.exceptionDetails) throw new Error(result.exceptionDetails.exception?.description ?? result.exceptionDetails.text);
      return result.result.value;
    }
    const loaded = await waitFor(() => evaluate('({url: location.href, ready: document.readyState})').catch(() => null),
      value => sameDocument(value?.url) && value.ready === 'complete', 30_000);
    if (!loaded) throw new Error('Live document did not finish loading');
    const { executionContextId } = await cmd('Page.createIsolatedWorld', { frameId: navigation.frameId, worldName: 'legacy-test262-observer' });
    const evaluateHere = expression => evaluate(expression, executionContextId);
    const state = () => evaluateHere(`(${observePage})(${JSON.stringify(counterIds)})`);
    async function capture(label, seen) {
      entry.snapshots[label] = { atMs: elapsed(), ...(seen ?? await state()) };
      await writeFile(resolve(dir, `${label}.html`), await evaluateHere('document.documentElement.outerHTML'));
    }
    async function wait(label, predicate, ms = 15_000, interval) {
      const seen = await waitFor(state, predicate, ms, interval);
      await capture(label, seen);
      if (!seen) throw new Error(`Timed out waiting for ${label}`);
      return seen;
    }
    async function click(selector, chapterLabel) {
      const args = chapterLabel === undefined ? [selector] : [selector, chapterLabel];
      const point = await waitFor(() => evaluateHere(`(${clickPoint})(...${JSON.stringify(args)})`), Boolean, 5000);
      if (!point) throw new Error(`${selector} never became a hit-testable pointer target`);
      entry.actions.push({ atMs: elapsed(), selector, chapterLabel, ...point });
      await cmd('Input.dispatchMouseEvent', { ...point, type: 'mousePressed', button: 'left', clickCount: 1 });
      await cmd('Input.dispatchMouseEvent', { ...point, type: 'mouseReleased', button: 'left', clickCount: 1 });
    }

    await capture('load');
    stage = 'site-initialization';
    const initialized = await wait('initialized', s => s.homeVisible && s.runTarget === '.content-tests' && count(s.counters.planned) > 0);
    stage = 'run-tab';
    await click('#run');
    await wait('run-tab', s => s.runSelected && s.runVisible && s.controls.btnRunAll);
    let chapterDeadline;
    if (options.chapter) {
      stage = 'chapter-selection';
      const ready = await wait('chapter-ready', s => findChapter(s.chapterLabels, options.chapter));
      entry.selection = findChapter(ready.chapterLabels, options.chapter);
      stage = 'chapter-run';
      chapterDeadline = performance.now() + options.timeoutMs;
      await click('#chapterSelector tr img[alt="Run"]', entry.selection.label);
    } else {
      entry.selection = { kind: 'all', planned: count(initialized.counters.planned) };
      stage = 'run-all';
      await click('#btnRunAll');
    }
    const runBudget = fallback => {
      if (!chapterDeadline) return fallback;
      const remaining = chapterDeadline - performance.now();
      if (remaining <= 0) throw new Error('Chapter run exceeded its observation window');
      return Math.min(fallback, remaining);
    };
    await wait('running', s => s.controls.btnPause || (options.chapter && s.progress === 'Testing complete!'), runBudget(15_000));
    stage = 'first-result';
    const first = await wait('first-result', s => count(s.counters.ran) > 0, runBudget(25_000), 15);
    if (!countersConsistent(first.counters)) throw new Error('First observed counters do not reconcile');
    if (count(first.counters.planned) !== entry.selection.planned) {
      throw new Error('Running test count does not match the selected scope');
    }
    let checkpoint;
    if (options.until === 'complete') {
      stage = 'completion';
      entry.completionPollTimeouts = 0;
      // A slow test can block the renderer longer than an individual CDP
      // command. Preserve its last responsive state and use the run budget.
      checkpoint = await waitFor(async () => {
        try {
          const observed = await state();
          entry.lastObserved = { atMs: elapsed(), ...observed };
          return observed;
        } catch (error) {
          if (!String(error).includes('Runtime.evaluate: command timed out')) throw error;
          entry.completionPollTimeouts++;
          return null;
        }
      }, s => s?.progress === 'Testing complete!', runBudget(options.timeoutMs), 250);
      if (!checkpoint || (chapterDeadline && performance.now() > chapterDeadline)) {
        throw new Error('Site did not reach its completion marker within the observation window');
      }
      await capture('complete', checkpoint);
    } else {
      stage = 'pause';
      await click('#btnPause');
      checkpoint = await wait('paused', s => s.controls.btnResume && s.controls.btnReset);
    }
    stage = 'counter-check';
    if (!countersConsistent(checkpoint.counters)) throw new Error('Checkpoint counters do not reconcile');
    if (options.until === 'complete' && !completeCounters(checkpoint.counters, entry.selection.planned)) {
      throw new Error('Site reported completion without running the selected planned tests');
    }
    stage = 'results';
    await click('#results');
    await wait('results', s => s.resultsVisible && ['ran', 'pass', 'fail', 'failedToLoad'].every(
      key => s.results[key] === checkpoint.counters[key]));
    if (options.until === 'first') {
      stage = 'reset';
      await click('#run');
      await click('#btnReset');
      await wait('reset', s => s.runVisible && s.controls.btnRunAll && count(s.counters.ran) === 0);
    }
    entry.websiteRun = options.until === 'complete' ? 'complete' : 'partial';
    stage = null;
  } catch (error) {
    entry.error = String(error);
  } finally {
    entry.firstFailedStage = stage;
    if (socket?.readyState === WebSocket.OPEN) {
      await send?.('Browser.close').catch(() => {});
    }
    socket?.close();
    const graceful = await Promise.race([done, delay(2500, null, { ref: false })]);
    if (!graceful && child.exitCode === null && child.signalCode === null) {
      // Restrict fallback cleanup to the browser process tree launched here.
      if (process.platform === 'win32' && child.pid) {
        await promisify(execFile)(resolve(process.env.SystemRoot, 'System32/taskkill.exe'),
          ['/PID', String(child.pid), '/T', '/F'], { windowsHide: true, timeout: 5000 }).catch(() => {});
      }
      if (child.exitCode === null && child.signalCode === null) child.kill();
    }
    entry.process = { ...await done, cleanup: graceful ? 'browser-close' : 'terminated-owned-process' };
  }
}

function parseOptions(argv) {
  const options = { output: 'target/test262/website', chromium: [], native: false, satinRunMs: 4000 };
  const keys = {
    '--satin': 'satin', '--output': 'output', '--until': 'until', '--chapter': 'chapter',
    '--timeout-ms': 'timeoutMs', '--satin-run-ms': 'satinRunMs',
  };
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === '--native') { options.native = true; continue; }
    if (!Object.hasOwn(keys, arg) && arg !== '--chromium') throw new Error(`Unknown option: ${arg}`);
    const value = argv[++i];
    if (!value || value.startsWith('--')) throw new Error(`Missing value for ${arg}`);
    if (arg === '--chromium') options.chromium.push(value);
    else options[keys[arg]] = arg.endsWith('-ms') ? Number(value) : value;
  }
  options.until ??= options.chapter ? 'complete' : 'first';
  options.timeoutMs ??= options.chapter ? 15_000 : 420_000;
  if (options.chapter && (options.satin || options.until !== 'complete' || !options.chapter.trim())) {
    throw new Error('--chapter requires Chromium and completion mode');
  }
  if ((!options.satin && !options.chromium.length) || (options.native && !options.satin) ||
      !['first', 'complete'].includes(options.until) ||
      ![options.timeoutMs, options.satinRunMs].every(ms => Number.isSafeInteger(ms) && ms > 0 && ms <= 1_800_000)) {
    throw new Error('Invalid arguments; see --help');
  }
  return options;
}

async function main(argv) {
  if (argv.includes('--help')) {
    console.log('node tools/test262/website.mjs [--satin FILE [--native] [--satin-run-ms MS]] [--chromium FILE ...] [--until first|complete | --chapter NAME (Chromium only)] [--timeout-ms MS] [--output DIR]');
    return;
  }
  const options = parseOptions(argv);
  await mkdir(resolve(options.output), { recursive: true });
  const output = await mkdtemp(resolve(options.output, 'run-'));
  const report = { schema: 1, scope: 'legacy-live-website', url: site, date: new Date().toISOString(), options, runs: {} };
  const jobs = [
    ...(options.satin ? [['satin', options.satin, satin]] : []),
    ...options.chromium.map((executable, i) => [`chromium-${i + 1}`, executable, chromium]),
  ];
  async function record(name, file, run) {
    const dir = resolve(output, name);
    const executable = resolve(file);
    const entry = report.runs[name] = { executable, websiteRun: 'unverified', snapshots: {}, actions: [] };
    const hash = async () => createHash('sha256').update(await readFile(executable)).digest('hex');
    try {
      await mkdir(dir);
      entry.sha256 = await hash();
      await run(executable, dir, options, entry);
      if (await hash() !== entry.sha256) throw new Error('Browser executable changed during capture');
    } catch (error) {
      entry.websiteRun = 'unverified';
      entry.firstFailedStage ??= 'probe';
      entry.probeError = String(error);
    }
  }
  await Promise.all(jobs.map(job => record(...job)));
  const path = resolve(output, 'report.json');
  await writeFile(path, JSON.stringify(report, null, 2) + '\n');
  console.log(JSON.stringify({
    report: path,
    runs: Object.fromEntries(Object.entries(report.runs).map(([name, run]) => [name, {
      websiteRun: run.websiteRun, firstFailedStage: run.firstFailedStage, firstDiagnostic: run.firstDiagnostic,
      selection: run.selection,
      counters: run.snapshots.complete?.counters ?? run.lastObserved?.counters ?? run.snapshots.paused?.counters ??
        run.snapshots['first-result']?.counters ?? run.load?.dom?.counters,
      error: run.probeError ?? run.error,
    }])),
  }));
  if (Object.values(report.runs).some(run => run.websiteRun === 'unverified')) process.exitCode = 1;
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  main(process.argv.slice(2)).catch(error => { console.error(String(error)); process.exitCode = 2; });
}
