// Verify network font admission and raster output through the browser executable.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { once } from 'node:events';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { inflateSync } from 'node:zlib';

const repo = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const args = process.argv.slice(2);
const options = new Map();
const positional = [];
for (let i = 0; i < args.length; i++) {
    const name = args[i];
    if (name === '--chrome-reference') options.set(name, true);
    else if (['--chrome', '--rea', '--timeout-ms'].includes(name)) {
        assert.ok(args[i + 1] && !args[i + 1].startsWith('--'), name + ' requires a value');
        options.set(name, args[++i]);
    } else {
        assert.ok(!name.startsWith('--'), 'Unknown option ' + name);
        positional.push(name);
    }
}
assert.ok(positional.length <= 2, 'Usage: node tools/verify-fonts.mjs [BROWSER.exe] [EVIDENCE_DIR] [--chrome-reference] [--chrome PATH] [--rea PATH] [--timeout-ms N]');
const binary = resolve(positional[0] ?? resolve(repo, 'target/debug/rust-browser.exe'));
const output = resolve(positional[1] ?? resolve(repo, 'target/font-verification'));
const timeout = Number(options.get('--timeout-ms') ?? 45000);
assert.ok(Number.isInteger(timeout) && timeout >= 10000 && timeout <= 180000);
await mkdir(output, { recursive: true });

const fontFixture = name => readFile(resolve(repo, 'crates/engine/tests/fixtures/fonts', name));
const [html, css, wide, fallback] = await Promise.all([
    readFile(new URL('./fixtures/fonts/index.html', import.meta.url), 'utf8'),
    readFile(new URL('./fixtures/fonts/redirected.css', import.meta.url), 'utf8'),
    fontFixture('wide.woff2'),
    fontFixture('fallback.ttf'),
]);
const malformed = Buffer.concat([Buffer.from('wOF2'), Buffer.alloc(28)]);
const sha256 = bytes => createHash('sha256').update(bytes).digest('hex');
const requests = [];
const state = Object.fromEntries(['rust', 'chrome'].map(mode => [mode, {
    measured: false,
    pending: [],
    release: [],
}]));
const expected = { wide: 32, alias: 32, denied: 20, reference: 20 };

function send(record, response, status, type, bytes) {
    record.status = status;
    response.statusCode = status;
    response.setHeader('Content-Type', type);
    response.end(bytes);
}

function log(request, role) {
    const record = {
        role,
        path: request.url,
        method: request.method,
        origin: request.headers.origin ?? null,
        status: null,
    };
    requests.push(record);
    return record;
}

let pageOrigin;
let assetOrigin;
const page = createServer((request, response) => {
    const record = log(request, 'document');
    const match = /^\/case\/(rust|chrome)\/document\/(index\.html|start\.css|measured)$/.exec(request.url);
    response.setHeader('Cache-Control', 'no-store');
    if (!match) return send(record, response, 404, 'text/plain', 'No document route');
    const [, mode, route] = match;
    if (route === 'index.html') return send(record, response, 200, 'text/html; charset=utf-8', html);
    if (route === 'start.css') {
        response.setHeader('Location', assetOrigin + '/case/' + mode + '/redirected/stylesheet.css');
        return send(record, response, 302, 'text/plain', '');
    }
    const phase = state[mode];
    phase.measured = true;
    for (const release of phase.pending.splice(0)) release('first-geometry');
    return send(record, response, 200, 'text/plain', 'measured');
});
const assets = createServer((request, response) => {
    const record = log(request, 'asset');
    const match = /^\/case\/(rust|chrome)\/redirected\/(stylesheet\.css|fonts\/(?:malformed\.woff2|wide\.woff2|denied\.woff2|fallback\.ttf))$/.exec(request.url);
    response.setHeader('Cache-Control', 'no-store');
    if (!match) return send(record, response, 404, 'text/plain', 'No asset route');
    const [, mode, route] = match;
    if (route !== 'fonts/denied.woff2') {
        response.setHeader('Access-Control-Allow-Origin', pageOrigin);
        response.setHeader('Vary', 'Origin');
    }
    if (route === 'stylesheet.css') return send(record, response, 200, 'text/css; charset=utf-8', css);
    if (route === 'fonts/malformed.woff2') return send(record, response, 200, 'font/woff2', malformed);
    if (route === 'fonts/denied.woff2') return send(record, response, 200, 'font/woff2', wide);
    if (route === 'fonts/fallback.ttf') return send(record, response, 200, 'font/ttf', fallback);
    const phase = state[mode];
    let released = false;
    const backup = setTimeout(() => release('watchdog'), 2500);
    backup.unref();
    function release(reason) {
        if (released || response.destroyed) return;
        released = true;
        clearTimeout(backup);
        phase.release.push(reason);
        setTimeout(() => {
            if (!response.destroyed) send(record, response, 200, 'font/woff2', wide);
        }, 60).unref();
    }
    if (phase.measured) release('first-geometry');
    else phase.pending.push(release);
});
async function listen(server) {
    server.listen(0, '127.0.0.1');
    await once(server, 'listening');
    return 'http://127.0.0.1:' + server.address().port;
}

async function start(exe, argv, name, deadline = timeout) {
    await writeFile(resolve(output, name + '-invocation.json'), JSON.stringify({ exe, argv, deadline }, null, 2));
    const child = spawn(exe, argv, { windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
    let stdout = '';
    let stderr = '';
    let limit = null;
    child.stdout.setEncoding('utf8').on('data', s => {
        stdout += s;
        if (stdout.length > 16 * 1024 * 1024) { limit = 'stdout limit'; child.kill(); }
    });
    child.stderr.setEncoding('utf8').on('data', s => {
        stderr += s;
        if (stderr.length > 2 * 1024 * 1024) { limit = 'stderr limit'; child.kill(); }
    });
    const timer = setTimeout(() => { limit = 'process deadline'; child.kill(); }, deadline);
    try {
        const [code] = await once(child, 'close');
        await Promise.all([
            writeFile(resolve(output, name + '-stdout.json'), stdout),
            writeFile(resolve(output, name + '-stderr.txt'), stderr),
        ]);
        assert.equal(limit, null, name + ': ' + limit);
        assert.equal(code, 0, name + ' exit ' + code + ': ' + stderr);
        return JSON.parse(stdout);
    } finally {
        clearTimeout(timer);
    }
}

function receipt(dom) {
    const pre = /<pre\b[^>]*\bid="result"[^>]*>([^<]+)<\/pre>/.exec(dom);
    assert.ok(pre && pre[0].includes('data-ready="yes"'), 'Missing final geometry receipt');
    return JSON.parse(pre[1]);
}

function verifyMeasurements(mode, data) {
    assert.equal(data.ok, true, mode + ' fixture observation: ' + JSON.stringify(data));
    assert.equal(data.loaded, true);
    assert.deepEqual(data.final, expected);
    assert.notEqual(data.first.wide, data.final.wide);
    assert.notEqual(data.first.alias, data.final.alias);
    assert.deepEqual(state[mode].release, ['first-geometry'], 'Font reply was held until first CSSOM read');
    const rows = requests.filter(row => row.path.startsWith('/case/' + mode + '/'));
    const path = suffix => rows.filter(row => row.path.endsWith(suffix));
    assert.deepEqual(path('/document/start.css').map(row => row.status), [302]);
    assert.deepEqual(path('/redirected/stylesheet.css').map(row => row.status), [200]);
    for (const name of ['malformed.woff2', 'wide.woff2', 'denied.woff2', 'fallback.ttf']) {
        assert.deepEqual(path('/redirected/fonts/' + name).map(row => [row.origin, row.status]), [[pageOrigin, 200]], name + ' must resolve from final CSS URL and be requested once with the document origin');
    }
    assert.ok(rows.indexOf(path('/fonts/malformed.woff2')[0]) < rows.indexOf(path('/fonts/wide.woff2')[0]), 'Fallback font source must be tried after the malformed source');
}

// Rust's headless encoder writes unfiltered or PNG-filtered 8-bit RGB(A).
// Decode just this bounded screenshot format for stable ink and background checks.
function readPixels(png, width, height) {
    assert.equal(png.subarray(0, 8).toString('hex'), '89504e470d0a1a0a');
    assert.equal(png.readUInt32BE(16), width);
    assert.equal(png.readUInt32BE(20), height);
    const channels = png[25] === 6 ? 4 : png[25] === 2 ? 3 : 0;
    assert.ok(channels && png[24] === 8 && png[28] === 0);
    const idat = [];
    for (let offset = 8; offset + 12 <= png.length;) {
        const length = png.readUInt32BE(offset);
        assert.ok(offset + length + 12 <= png.length);
        if (png.toString('ascii', offset + 4, offset + 8) === 'IDAT') idat.push(png.subarray(offset + 8, offset + 8 + length));
        offset += length + 12;
    }
    const stride = width * channels;
    const raw = inflateSync(Buffer.concat(idat), { maxOutputLength: (stride + 1) * height });
    assert.equal(raw.length, (stride + 1) * height);
    const pixels = Buffer.alloc(stride * height);
    function paeth(a, b, c) {
        const p = a + b - c;
        const da = Math.abs(p - a), db = Math.abs(p - b), dc = Math.abs(p - c);
        return da <= db && da <= dc ? a : db <= dc ? b : c;
    }
    for (let y = 0; y < height; y++) {
        const base = y * (stride + 1);
        const filter = raw[base];
        assert.ok(filter <= 4);
        for (let x = 0; x < stride; x++) {
            const i = y * stride + x;
            const a = x >= channels ? pixels[i - channels] : 0;
            const b = y ? pixels[i - stride] : 0;
            const c = y && x >= channels ? pixels[i - stride - channels] : 0;
            const predictor = filter === 0 ? 0 : filter === 1 ? a : filter === 2 ? b : filter === 3 ? (a + b) >> 1 : paeth(a, b, c);
            pixels[i] = (raw[base + 1 + x] + predictor) & 255;
        }
    }
    return (x, y) => [...pixels.subarray((y * width + x) * channels, (y * width + x) * channels + 3)];
}

const report = { ok: false, binary, startedAt: new Date().toISOString(), requests, failures: [], fontFiles: {
    wide: { bytes: wide.length, sha256: sha256(wide) },
    fallback: { bytes: fallback.length, sha256: sha256(fallback) },
}};
async function verify(name, check) {
    try { await check(); } catch (error) { report.failures.push({ name, error: String(error.stack ?? error) }); }
}

try {
    [pageOrigin, assetOrigin] = await Promise.all([listen(page), listen(assets)]);
    Object.assign(report, { pageOrigin, assetOrigin });
    await verify('Rust driver and renderer', async () => {
        report.binarySha256 = sha256(await readFile(binary));
        const domPath = resolve(output, 'rust-final.html');
        const pngPath = resolve(output, 'rust-final.png');
        report.rust = { process: await start(binary, [
            '--headless', '--url', pageOrigin + '/case/rust/document/index.html', '--width', '240', '--height', '300',
            '--run-for-ms', '4000', '--dump-dom', domPath, '--screenshot', pngPath,
        ], 'rust') };
        report.rust.measurements = receipt(await readFile(domPath, 'utf8'));
        verifyMeasurements('rust', report.rust.measurements);
        assert.equal(report.rust.process.title, 'Font pipeline OK');
        assert.equal(report.rust.process.layout_limited, false);
        assert.equal(report.rust.process.paint_limited, false);
        const png = await readFile(pngPath);
        const pixel = readPixels(png, 240, 300);
        report.rust.ink = { wide: pixel(45, 36), alias: pixel(45, 91), denied: pixel(45, 146), reference: pixel(45, 201) };
        assert.deepEqual(report.rust.ink, {
            wide: [0, 0, 0], alias: [0, 0, 0], denied: [255, 255, 255], reference: [255, 255, 255],
        }, 'Wider decoded face must paint ink beyond the fallback outline');
        report.rust.screenshotSha256 = sha256(png);
    });

    if (options.get('--chrome-reference')) await verify('Owned Chrome through REA', async () => {
        const browser = resolve(options.get('--chrome') ?? resolve(process.env.ProgramFiles ?? 'C:/Program Files', 'Google/Chrome/Application/chrome.exe'));
        const rea = resolve(options.get('--rea') ?? process.env.REA_CLI ?? resolve(repo, 'node_modules/rea-agents/scripts/rea.mjs'));
        const scenario = {
            browser: { mode: 'launch', executable_path: browser, headless: true },
            start_url: { url: pageOrigin + '/case/chrome/document/index.html' },
            actions: [{
                step_id: 'font-receipt', action: 'wait_for', timeout_ms: 10000,
                locator: { kind: 'css', selector: '#result[data-ready="yes"]' }, state: 'attached',
            }],
            capture: { at_end: ['url', 'dom'], events: ['page-errors', 'network'] },
            command_timeout_ms: 45000,
        };
        const scenarioPath = resolve(output, 'chrome-scenario.json');
        await writeFile(scenarioPath, JSON.stringify(scenario, null, 2));
        const result = await start(process.execPath, [rea, 'capture-browser-scenario', scenarioPath, '--format', 'json', '--full-output'], 'chrome-rea', Math.max(timeout, 90000));
        const capture = result.data?.normalized_result;
        assert.equal(result.ok, true);
        assert.ok(capture.steps.every(step => step.status === 'completed'));
        assert.equal(capture.browser.cleanup, 'terminated-owned-process');
        assert.equal(capture.browser.process_ownership, 'provider-owned');
        const dom = capture.steps.at(-1).artifacts.dom;
        assert.equal(dom.state, 'captured');
        report.chrome = { evidenceId: result.data.evidence_id, browser: capture.browser, measurements: receipt(dom.value.text) };
        await writeFile(resolve(output, 'chrome-final.html'), dom.value.text);
        verifyMeasurements('chrome', report.chrome.measurements);
        if (report.rust?.measurements?.ok) assert.deepEqual(report.chrome.measurements.final, report.rust.measurements.final);
    });
} catch (error) {
    report.failures.push({ name: 'Fixture setup', error: String(error.stack ?? error) });
} finally {
    report.ok = report.failures.length === 0;
    report.releases = Object.fromEntries(Object.entries(state).map(([mode, phase]) => [mode, phase.release]));
    await Promise.all([
        writeFile(resolve(output, 'fixture.html'), html),
        writeFile(resolve(output, 'redirected.css'), css),
        writeFile(resolve(output, 'report.json'), JSON.stringify(report, null, 2)),
    ]);
    for (const server of [page, assets]) server.closeAllConnections();
    await Promise.all([page, assets].map(server => new Promise(done => server.close(done))));
}
if (!report.ok) process.exitCode = 1;
console.log(JSON.stringify({ ok: report.ok, measurements: report.rust?.measurements, chrome: report.chrome?.measurements, evidence: report.chrome?.evidenceId, report: resolve(output, 'report.json'), failures: report.failures }));
