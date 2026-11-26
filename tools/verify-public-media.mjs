import { execFile } from 'node:child_process';
import { createHash } from 'node:crypto';
import { createReadStream } from 'node:fs';
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
import { arch, release } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { budgetTrace, executionProfiles, safeDiagnostic } from './public-media-diagnostics.mjs';

const cases = [
  { site: 'twitch', url: 'https://www.twitch.tv/theburntpeanut_247' },
  { site: 'youtube', url: 'https://www.youtube.com/watch?v=aqz-KE-bpKQ' },
];
const viewport = { width: 1280, height: 800 };
const player = {
  twitch: {
    shell: '[data-a-target="video-player"],.video-player',
    play: 'button[data-a-target="player-play-pause-button"]',
    seek: '[data-a-target="player-seekbar"]',
  },
  youtube: { shell: '#movie_player', play: '#movie_player .ytp-play-button', seek: '#movie_player .ytp-progress-bar' },
};
async function sha256(path) {
  const hash = createHash('sha256');
  for await (const chunk of createReadStream(path)) hash.update(chunk);
  return hash.digest('hex');
}

const capture = promisify(execFile);
async function run(exe, args, timeout, env) {
  const options = { timeout, env, windowsHide: true, maxBuffer: 16 * 1024 * 1024 };
  try {
    const { stdout, stderr } = await capture(exe, args, options);
    return { stdout, stderr, exit: 0, killed: false };
  } catch (error) {
    return {
      stdout: error.stdout ?? '',
      stderr: error.stderr ?? '',
      exit: Number.isInteger(error.code) ? error.code : null,
      killed: error.killed ?? false,
    };
  }
}

async function satinCase(exe, { site, url }, dir) {
  const started = new Date();
  const dom = join(dir, 'transient.html');
  const profile = join(dir, 'profile');
  const observationMs = 8000;
  const args = [
    '--headless', '--url', url, '--profile', profile, '--dump-dom', dom,
    '--width', String(viewport.width), '--height', String(viewport.height),
    '--run-for-ms', String(observationMs),
  ];
  const outcome = await run(exe, args, 65000);
  let result;
  try { result = JSON.parse(outcome.stdout); } catch { /* no JSON on a failed load */ }
  let document = null;
  const html = await readFile(dom, 'utf8').catch(() => null);
  if (html !== null) {
    // Only count opening tags outside raw-text content, templates and comments.
    const visible = html.replace(/<!--[\s\S]*?-->|<(script|style|template|textarea|title|xmp|noscript)\b[^>]*>[\s\S]*?<\/\1\s*>/gi, '');
    document = {
      htmlBytes: Buffer.byteLength(html),
      videoElements: (visible.match(/<video(?=[\s/>])/gi) ?? []).length,
      playerMarker: site === 'youtube' ? /\bid=['"]movie_player['"]/i.test(visible)
        : /\bdata-a-target=['"](?:video-player|player-play-pause-button)['"]/i.test(visible),
      chatLineMarkers: site === 'twitch'
        ? (visible.match(/\bdata-(?:a-target|test-selector)=['"]chat-line-message['"]/gi) ?? []).length : null,
    };
  }
  const diagnostics = result?.javascript?.diagnostics;
  const media = result?.media?.map(status => ({
    node: status.node, current_time: status.current_time, ready_state: status.ready_state,
    video_width: status.video_width, video_height: status.video_height,
    has_audio: status.has_audio, decoded_frames: status.decoded_frames, paused: status.paused, ended: status.ended,
  })) ?? null;
  return {
    startedUtc: started.toISOString(), elapsedMs: Date.now() - started.getTime(), site, url,
    exit: outcome.exit, killed: outcome.killed, requestedRunForMs: observationMs, version: result?.version ?? null,
    finalHost: result?.url ? new URL(result.url).hostname : null,
    executedScripts: result?.javascript?.executed_scripts ?? null,
    diagnosticCount: diagnostics?.length ?? null,
    nestingDiagnostics: diagnostics?.filter(line => line.includes('Expression nesting limit exceeded')).length ?? null,
    firstDiagnostics: diagnostics?.slice(0, 8).map(safeDiagnostic) ?? null,
    budgetTrace: budgetTrace(outcome.stderr),
    executionProfiles: executionProfiles(outcome.stderr),
    document, media,
    timeProgress: 'not sampled within one run',
    controls: 'not tried by this headless probe',
  };
}

async function sample(page, site) {
  return page.evaluate(({ site, selector }) => {
    const root = document.querySelector(selector);
    const videos = [...document.querySelectorAll('video')];
    const video = root?.querySelector('video') ?? videos[0];
    const quality = video?.getVideoPlaybackQuality?.();
    const finite = n => Number.isFinite(n) ? Math.round(n * 1000) / 1000 : null;
    const messages = site === 'twitch' ? document.querySelectorAll(
      '[data-a-target="chat-line-message"],[data-test-selector="chat-line-message"],.chat-line__message',
    ).length : null;
    return {
      host: location.hostname, videoElements: videos.length, playerVideo: !!root?.querySelector('video'),
      playerShell: !!root, adMarker: !!document.querySelector('#movie_player.ad-showing,[data-a-target="video-ad-label"]'),
      consentVisible: site === 'youtube' && (location.hostname === 'consent.youtube.com'
        || [...document.querySelectorAll('button')].some(button =>
          /reject (all|the use of cookies)|odrzuć wszystko/i.test(`${button.textContent} ${button.getAttribute('aria-label')}`)
          && button.getClientRects().length && getComputedStyle(button).visibility === 'visible')),
      youtubeAccessWall: site === 'youtube' && /sign in to confirm|not a bot|not available/i.test(
        (document.querySelector('.ytp-error-content-wrap,ytd-enforcement-message-view-model')?.textContent ?? ''),
      ),
      twitchGate: site === 'twitch' && !!document.querySelector(
        '[data-a-target="player-overlay-content-gate"],[data-a-target="content-classification-gate-overlay-start-watching-button"]',
      ),
      liveIndicator: site === 'twitch' && !!document.querySelector(
        '[data-a-target="player-live-indicator"],[data-a-target="stream-status"]',
      ),
      chatMessagesRendered: messages,
      video: video ? {
        currentTime: finite(video.currentTime), duration: finite(video.duration),
        durationInfinite: video.duration === Infinity, paused: video.paused, ended: video.ended,
        readyState: video.readyState, width: video.videoWidth, height: video.videoHeight,
        totalVideoFrames: quality?.totalVideoFrames ?? null, droppedVideoFrames: quality?.droppedVideoFrames ?? null,
        decodedVideoFrames: video.webkitDecodedFrameCount ?? null,
        decodedAudioBytes: video.webkitAudioDecodedByteCount ?? null, muted: video.muted, volume: video.volume,
        seekableRanges: Array.from({ length: video.seekable.length }, (_, i) => [
          finite(video.seekable.start(i)), finite(video.seekable.end(i)),
        ]),
        mediaErrorCode: video.error?.code ?? null,
      } : null,
    };
  }, { site, selector: player[site].shell });
}

async function chromeCase(exe, chromium, { site, url }) {
  const startedUtc = new Date().toISOString();
  const outcome = { site, url, startedUtc, samples: [], controls: {}, pageErrorCount: 0, firstErrors: [] };
  const browser = await chromium.launch({ executablePath: exe, headless: true });
  try {
    outcome.version = browser.version();
    const context = await browser.newContext({ viewport, locale: 'en-US', timezoneId: 'UTC', serviceWorkers: 'allow' });
    const page = await context.newPage();
    const session = await context.newCDPSession(page);
    const audio = new Map();
    const observedScripts = new Set();
    const inPage = new Set();
    outcome.dedicatedWorkers = { total: 0, blob: 0 };
    page.on('worker', worker => {
      outcome.dedicatedWorkers.total++;
      if (worker.url().startsWith('blob:')) outcome.dedicatedWorkers.blob++;
    });
    session.on('Debugger.scriptParsed', ({ scriptId, url: source }) => {
      if (/^https?:/.test(source)) inPage.add(scriptId);
    });
    session.on('Media.playerPropertiesChanged', ({ playerId, properties }) => {
      const track = audio.get(playerId) ?? {};
      for (const { name, value } of properties) {
        if (name === 'kAudioDecoderName' && /^[A-Za-z0-9_]{1,60}$/.test(value)) track.decoder = value;
        if (name === 'kAudioTracks' || name === 'kVideoTracks') {
          try {
            const tracks = JSON.parse(value);
            if (Array.isArray(tracks)) track[name === 'kAudioTracks' ? 'audioTracks' : 'videoTracks'] = tracks.length;
          } catch { /* media UI properties are optional */ }
        }
      }
      audio.set(playerId, track);
    });
    page.on('pageerror', error => {
      outcome.pageErrorCount++;
      if (outcome.firstErrors.length < 8) outcome.firstErrors.push(safeDiagnostic(`${error.name}: ${error.message}`));
    });
    const attempts = await Promise.allSettled([
      session.send('Debugger.enable'), session.send('Profiler.enable'), session.send('Media.enable'),
    ]);
    const coverage = attempts[0].status === 'fulfilled' && attempts[1].status === 'fulfilled'
      && await session.send('Profiler.startPreciseCoverage', { callCount: true, detailed: false }).then(() => true, () => false);
    const countScripts = async () => {
      if (!coverage) return null;
      const data = await session.send('Profiler.takePreciseCoverage').catch(() => null);
      if (!data) return null;
      for (const script of data.result) {
        if (inPage.has(script.scriptId) && script.functions.some(fn => fn.ranges.some(range => range.count > 0))) {
          observedScripts.add(script.scriptId);
        }
      }
      // Counts HTTP(S)-named script IDs whose functions ran in this page's CDP target;
      // it is not a count of <script> tags or workers, nor the Satin loader counter.
      return observedScripts.size;
    };
    const record = async label => {
      const state = await sample(page, site).catch(() => null);
      outcome.samples.push({ label, utc: new Date().toISOString(), ...state });
      return state;
    };
    const click = async locator => locator.first().click({ timeout: 3500 }).then(() => true, () => false);
    const play = page.locator(`${player[site].play}:visible`);
    outcome.navigationStatus = await page.goto(url, { waitUntil: 'domcontentloaded', timeout: 18000 })
      .then(response => response?.status() ?? null, () => null);
    const reject = page.locator('button:visible').filter({ hasText: /reject all|odrzuć wszystko/i })
      .or(page.getByRole('button', { name: /reject (all|the use of cookies)|odrzuć wszystko/i }));
    if (site === 'youtube') outcome.controls.rejectOptionalCookies = await click(reject);
    await page.locator(player[site].shell).first().waitFor({ state: 'attached', timeout: 15000 }).catch(() => {});
    if (site === 'twitch') {
      await page.waitForFunction(selector => {
        const video = document.querySelector(selector)?.querySelector('video');
        return video?.readyState >= 1;
      }, player[site].shell, { timeout: 12000 }).catch(() => {});
    } else if (!outcome.controls.rejectOptionalCookies) outcome.controls.rejectOptionalCookies = await click(reject);
    if (outcome.controls.rejectOptionalCookies) {
      await reject.first().waitFor({ state: 'hidden', timeout: 5000 }).catch(() => {});
    }
    const opening = await record('initial');
    if (opening?.video && !opening.video.paused) {
      outcome.controls.play = 'autoplay already active';
    } else {
      outcome.controls.play = await click(play) ? 'clicked' : 'play control unavailable';
    }
    const waitForPlaying = timeout => page.waitForFunction(selector => {
      const video = document.querySelector(selector)?.querySelector('video');
      return video && !video.paused && video.readyState >= 2 && video.currentTime > 0;
    }, player[site].shell, { timeout, polling: 200 }).then(() => true, () => false);
    const playing = await waitForPlaying(18000);
    let start = await record('progress-start');
    if (!playing && site === 'youtube' && start?.video?.paused && !start.youtubeAccessWall && !start.consentVisible) {
      outcome.controls.playRetry = await click(play) ? 'clicked' : 'play control unavailable';
      await waitForPlaying(8000);
      start = await record('after-play-retry');
    }
    await countScripts();
    if (start?.video) {
      await page.waitForTimeout(4500);
      const end = await record('progress-end');
      if (end?.video && !end.video.paused) {
        outcome.controls.pause = await click(play) ? 'clicked' : 'pause control unavailable';
        const paused = await record('after-pause');
        await page.waitForTimeout(1200);
        await record('pause-hold');
        if (paused?.video?.paused) {
          outcome.controls.resume = await click(play) ? 'clicked' : 'resume control unavailable';
          await page.waitForTimeout(1800);
          await record('after-resume');
        }
      }
      const bar = page.locator(player[site].seek).first();
      const before = await sample(page, site).catch(() => null);
      const box = !before?.adMarker && before?.video?.seekableRanges.length && await bar.boundingBox({ timeout: 1500 }).catch(() => null);
      if (box?.width > 0 && box.height > 0) {
        outcome.controls.seek = await bar.click({ position: { x: box.width * 0.5, y: box.height * 0.5 }, timeout: 3500 })
          .then(() => 'clicked midpoint', () => 'seek click failed');
        await page.waitForTimeout(1800);
        await record('after-seek');
      } else outcome.controls.seek = 'not tried: no usable seek control and range, or ad present';
    }
    outcome.executedScripts = await countScripts();
    outcome.audio = attempts[2].status === 'fulfilled' ? [...audio.values()].filter(track => Object.keys(track).length) : null;
    return outcome;
  } finally {
    await browser.close();
  }
}

async function reaCase(chrome, cli, { site, url }, dir) {
  const source = new URL(url);
  const scenario = {
    browser: { mode: 'launch', executable_path: chrome, headless: true },
    start_url: {
      url: source.origin + source.pathname,
      query: [...source.searchParams].map(([name, value]) => ({ name, value: { source: 'literal', value } })),
    },
    environment: { viewport: { ...viewport, device_scale_factor: 1 }, service_workers: 'allow' },
    actions: [{
      step_id: 'media-shell', action: 'wait_for', state: 'attached', timeout_ms: 15000,
      locator: { kind: 'css', selector: `${player[site].shell},form[action*="consent.youtube.com"]` },
    }, { step_id: 'settle', action: 'wait_for_timeout', duration_ms: 4000 }],
    capture: { at_end: ['url'], events: [] },
    command_timeout_ms: 40000,
  };
  const path = join(dir, 'scenario.json');
  await writeFile(path, JSON.stringify(scenario));
  const startedUtc = new Date().toISOString();
  const runResult = await run(process.execPath, [cli, 'capture-browser-scenario', path, '--json', '--full-output'], 60000,
    { ...process.env, TEMP: dir, TMP: dir });
  let result;
  try { result = JSON.parse(runResult.stdout); } catch { /* no raw output retained */ }
  const evidence = result?.data ?? result;
  const contents = evidence?.normalized_result;
  const { stdout: _stdout, stderr: _stderr, ...status } = runResult;
  return {
    site, url, startedUtc, ...status, ok: result?.ok ?? null,
    evidenceId: evidence?.evidence_id ?? null,
    browser: contents?.browser ? {
      mode: contents.browser.mode, version: contents.browser.version, cleanup: contents.browser.cleanup,
    } : null,
    steps: contents?.steps?.map(step => ({ id: step.step_id, status: step.status })) ?? null,
  };
}

const [mode, executable, identifier, site] = process.argv.slice(2);
const selected = cases.filter(test => !site || test.site === site);
if (!['chrome', 'satin'].includes(mode) || !executable || !identifier || !selected.length) {
  console.error('Usage: node tools/verify-public-media.mjs chrome <chrome.exe> <absolute rea.mjs> [twitch|youtube]');
  console.error('   or: node tools/verify-public-media.mjs satin <rust-browser.exe> <git commit> [twitch|youtube]');
  process.exit(1);
}
const root = fileURLToPath(new URL('../target/public-media/', import.meta.url));
await mkdir(root, { recursive: true });
const directory = await mkdtemp(join(root, `${mode}-`));
const temporary = join(directory, 'transient');
await mkdir(temporary);
const exe = resolve(executable);
const binary = { path: exe, sha256: await sha256(exe), commit: mode === 'satin' ? identifier : null };
const report = {
  mode, binary, viewport, node: process.version, os: process.platform, osRelease: release(), arch: arch(),
  startedUtc: new Date().toISOString(), cases: [], rea: [],
};
const output = join(directory, 'summary.json');
async function save() { await writeFile(output, JSON.stringify(report, null, 2) + '\n'); }
const tempKeys = ['TEMP', 'TMP'];
const previousTemp = tempKeys.map(key => process.env[key]);
try {
  for (const key of tempKeys) process.env[key] = temporary;
  if (mode === 'chrome') {
    const cli = resolve(identifier);
    const require = createRequire(cli);
    const { chromium } = require('playwright-core');
    report.reaVersion = JSON.parse(await readFile(join(dirname(cli), '..', 'package.json'), 'utf8')).version;
    for (const test of selected) {
      report.rea.push(await reaCase(exe, cli, test, temporary));
      await save();
      report.cases.push(await chromeCase(exe, chromium, test));
      await save();
    }
  } else {
    for (const test of selected) {
      await mkdir(join(temporary, test.site));
      report.cases.push(await satinCase(exe, test, join(temporary, test.site)));
      await save();
    }
  }
} catch (error) {
  report.runnerError = safeDiagnostic(`${error.name}: ${error.message}`);
  await save();
  process.exitCode = 1;
} finally {
  tempKeys.forEach((key, i) => {
    if (previousTemp[i] === undefined) delete process.env[key];
    else process.env[key] = previousTemp[i];
  });
  await rm(temporary, { recursive: true, force: true, maxRetries: 5 });
}
console.log(output);
