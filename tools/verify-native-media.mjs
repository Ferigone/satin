// Run the Windows Media Foundation backend headlessly against authored loopback media.
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { createHash } from 'node:crypto';
import { once } from 'node:events';
import { mkdir, mkdtemp, readFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';

assert.ok(process.argv.length <= 4,
  'Usage: node tools/verify-native-media.mjs [BROWSER.exe] [FRAGMENT_FIXTURE_DIRECTORY]');
const root = fileURLToPath(new URL('..', import.meta.url));
const binary = resolve(process.argv[2] ?? join(root, 'target/debug/rust-browser.exe'));
const pages = join(root, 'crates/engine/data/pages');
const fixtures = resolve(process.argv[3] ?? pages);
await mkdir(join(root, 'target'), { recursive: true });
const output = await mkdtemp(join(root, 'target/native-media-'));
const run = promisify(execFile);

// These fixtures use a single ftyp/moov initialization segment and moof/mdat pairs.
// The original fixture files may additionally include an mfra file index, not a media fragment.
function splitFixture(bytes) {
  const boxes = [];
  for (let start = 0; start < bytes.length;) {
    const size = bytes.readUInt32BE(start);
    assert.ok(size >= 8 && start + size <= bytes.length, 'Invalid fixture MP4 box');
    boxes.push({ type: bytes.toString('ascii', start + 4, start + 8), start, end: start + size });
    start += size;
  }
  if (boxes.at(-1)?.type === 'mfra') boxes.pop();
  assert.equal(boxes.length, 12, 'Expected initialization and five media segments');
  return Array.from({ length: 6 }, (_, index) => {
    const [first, second] = boxes.slice(index * 2, index * 2 + 2);
    assert.deepEqual([first.type, second.type], index ? ['moof', 'mdat'] : ['ftyp', 'moov']);
    return bytes.subarray(first.start, second.end);
  });
}

const progressive = await readFile(join(pages, 'media-demo.mp4'));
const tracks = Object.fromEntries(await Promise.all(['video', 'audio'].map(async track =>
  [track, splitFixture(await readFile(join(fixtures, `mse-${track}.mp4`)))])));
const modes = ['progressive', 'open', 'finalized', 'rebuffer'];

function page(mode) {
  return `<!doctype html><meta charset="utf-8"><title>Native media: ${mode}</title>
<video id="clip" muted preload="none" width="240" height="136"></video>
<button id="start">Play fixture</button><pre id="trace"></pre>
<script>
const mode = ${JSON.stringify(mode)};
const clip = document.getElementById('clip');
const trace = document.getElementById('trace');
function check(yes, message) { if (!yes) throw new Error(message); }
function note(step) {
  trace.textContent += step + ' at ' + clip.currentTime.toFixed(3) + ', ready ' + clip.readyState + '\\n';
}
function fail(error) {
  note(String(error));
  document.title = 'Native media failed: ' + error;
}
function event(target, name) {
  return new Promise(resolve => target.addEventListener(name, resolve, {once: true}));
}
function delay(ms) { return new Promise(resolve => setTimeout(resolve, ms)); }
async function advancePast(time) {
  while (clip.currentTime <= time) await event(clip, 'timeupdate');
}
async function append(buffer, track, index) {
  const response = await fetch('/' + track + '/' + index);
  check(response.ok, 'Segment fetch failed');
  const bytes = await response.arrayBuffer();
  const ended = new Promise((resolve, reject) => {
    function clean() {
      buffer.removeEventListener('error', onError);
      buffer.removeEventListener('updateend', onEnd);
    }
    function onError() { clean(); reject(new Error('append failed: ' + track + '/' + index)); }
    function onEnd() { clean(); resolve(); }
    buffer.addEventListener('error', onError);
    buffer.addEventListener('updateend', onEnd);
  });
  buffer.appendBuffer(bytes);
  check(buffer.updating, 'append did not start');
  await ended;
  check(!buffer.updating, 'append did not finish');
}
async function pauseAndSeek(time) {
  clip.pause();
  // Allow any in-flight native clock sample to arrive before measuring a paused period.
  await delay(100);
  const stopped = clip.currentTime;
  await delay(250);
  check(clip.paused && Math.abs(clip.currentTime - stopped) < 0.12, 'Clock did not pause');
  note('pause');
  const seeked = event(clip, 'seeked');
  clip.currentTime = time;
  check(clip.seeking, 'No seek pending');
  await seeked;
  check(clip.paused && !clip.seeking && Math.abs(clip.currentTime - time) < 0.15, 'Paused seek missed');
  note('seek');
  await clip.play();
  await advancePast(time + 0.2);
  check(!clip.paused && clip.readyState >= 2, 'Playback did not resume');
  note('resume');
}
async function start() {
  let source, video, audio;
  if (mode === 'progressive') {
    clip.src = '/progressive.mp4';
  } else {
    source = new MediaSource();
    const opened = event(source, 'sourceopen');
    const url = URL.createObjectURL(source);
    clip.src = url;
    await opened;
    URL.revokeObjectURL(url);
    video = source.addSourceBuffer('video/mp4; codecs="avc1.42C01E"');
    audio = source.addSourceBuffer('audio/mp4; codecs="mp4a.40.2"');
    for (let i = 0; i < 5; i++) {
      await Promise.all([append(video, 'video', i), append(audio, 'audio', i)]);
    }
    check(source.activeSourceBuffers.length === 2, 'Both tracks should be active');
    check(clip.buffered.length === 1 && clip.buffered.end(0) > 2.5, 'No useful buffered interval');
    note('initial append complete');
  }
  await clip.play();
  await advancePast(0.3);
  check(clip.videoWidth > 0 && clip.videoHeight > 0 && clip.readyState >= 2, 'No playable video');
  note('playing');
  if (mode === 'progressive') {
    await pauseAndSeek(2);
  } else if (mode === 'open') {
    check(source.readyState === 'open', 'Expected a live-like open source');
    await pauseAndSeek(1.1);
    check(source.readyState === 'open' && !clip.ended, 'Open source unexpectedly ended');
  } else if (mode === 'rebuffer') {
    const edge = clip.buffered.end(0);
    await event(clip, 'waiting');
    note('waiting at buffered edge ' + edge);
    check(clip.readyState < 3 && !clip.paused, 'Reported future data while waiting');
    await Promise.all([append(video, 'video', 5), append(audio, 'audio', 5)]);
    // Queue another four seconds past each track's edge, without ending the source.
    video.timestampOffset = video.buffered.end(0);
    audio.timestampOffset = audio.buffered.end(0);
    for (let i = 1; i < 5; i++) {
      await Promise.all([append(video, 'video', i), append(audio, 'audio', i)]);
    }
    note('refilled');
    await advancePast(edge + 0.8);
    check(source.readyState === 'open' && !clip.paused && clip.readyState >= 3, 'Did not recover from rebuffering');
    note('clock recovered');
  } else {
    const previousEnd = clip.buffered.end(0);
    await Promise.all([append(video, 'video', 5), append(audio, 'audio', 5)]);
    check(clip.buffered.end(0) > previousEnd + 0.4, 'Final append did not extend the range');
    note('final append complete');
    const sourceEnded = event(source, 'sourceended');
    source.endOfStream();
    await sourceEnded;
    check(source.readyState === 'ended', 'Source did not finalize');
    const ended = event(clip, 'ended');
    await pauseAndSeek(1.6);
    await ended;
    check(clip.ended && clip.paused, 'Element did not end');
    note('ended');
  }
  if (!clip.ended) clip.pause();
  document.title = 'Native media OK: ' + mode;
}
clip.addEventListener('error', () => fail('media element error'));
document.getElementById('start').addEventListener('click', () => start().catch(fail));
</script>`;
}

const requests = [];
const server = createServer((request, response) => {
  const path = request.url;
  requests.push(path);
  const segment = /^\/(video|audio)\/([0-5])$/.exec(path);
  const body = path === '/progressive.mp4' ? progressive
    : segment ? tracks[segment[1]][Number(segment[2])]
    : modes.includes(path?.slice(1)) ? page(path.slice(1)) : null;
  if (body === null) return response.writeHead(404).end();
  response.writeHead(200, {
    'Content-Type': segment ? 'application/octet-stream'
      : path === '/progressive.mp4' ? 'video/mp4' : 'text/html; charset=utf-8',
    'Content-Length': Buffer.byteLength(body),
    'Cache-Control': 'no-store',
  });
  response.end(body);
});
server.listen(0, '127.0.0.1');
await once(server, 'listening');
const origin = `http://127.0.0.1:${server.address().port}`;
const checks = [];
try {
  for (const mode of modes) {
    requests.length = 0;
    const dom = join(output, `${mode}.html`);
    const { stdout } = await run(binary, [
      '--headless', '--url', origin + '/' + mode, '--click-id', 'start',
      '--profile', join(output, `${mode}-profile`), '--dump-dom', dom,
      '--run-for-ms', mode === 'finalized' || mode === 'rebuffer' ? '7800' : '5200',
      '--width', '420', '--height', '440',
    ], { timeout: 20000, windowsHide: true });
    const result = JSON.parse(stdout);
    const html = await readFile(dom, 'utf8');
    const trace = html.match(/<pre id="trace">([\s\S]*?)<\/pre>/)?.[1];
    const [media] = result.media;
    checks.push({ mode, title: result.title, media, trace, requests: [...requests] });
    assert.deepEqual(result.javascript.diagnostics, [], `${mode}: ${trace}`);
    assert.equal(result.title, `Native media OK: ${mode}`, trace);
    assert.equal(result.media.length, 1);
    assert.ok(media.has_audio);
    assert.deepEqual([media.video_width, media.video_height],
      mode === 'progressive' ? [320, 180] : [160, 90]);
    assert.ok(media.ready_state >= 2 && media.decoded_frames >= 8);
    assert.ok(media.current_time > (mode === 'progressive' ? 2.2 : 1.3));
    assert.equal(media.paused, true);
    assert.equal(media.ended, mode === 'finalized');
    const expected = mode === 'progressive' ? ['/progressive.mp4']
      : Object.keys(tracks).flatMap(track =>
        Array.from({ length: mode === 'open' ? 5 : 6 }, (_, index) => `/${track}/${index}`));
    if (mode === 'rebuffer') {
      expected.push(...expected.filter(path => /\/[1-4]$/.test(path)));
    }
    assert.deepEqual(requests.filter(path => expected.includes(path)).sort(), expected.sort());
    if (mode === 'open') assert.ok(!requests.some(path => /\/5$/.test(path)));
  }
} finally {
  console.log(JSON.stringify({
    binarySha256: createHash('sha256').update(await readFile(binary)).digest('hex'),
    fixtures, output, checks,
  }, null, 2));
  const closed = once(server, 'close');
  server.close();
  server.closeAllConnections();
  await closed;
}
