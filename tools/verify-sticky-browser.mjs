import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import http from 'node:http';
import { resolve } from 'node:path';
import { promisify } from 'node:util';
import { inflateSync } from 'node:zlib';

const binary = resolve(process.argv[2] ?? 'target/debug/rust-browser.exe');
const output = resolve(process.argv[3] ?? 'target/sticky-verification');
const argument = name => {
    const index = process.argv.indexOf(name);
    return index < 0 ? undefined : process.argv[index + 1];
};
const rea = argument('--rea') ?? process.env.REA_CLI;
const chrome = argument('--chrome') ?? 'C:/Program Files/Google/Chrome/Application/chrome.exe';
const chromeOnly = process.argv.includes('--chrome-only');
assert.ok(rea, 'Supply the REA CLI script via --rea or REA_CLI');

const path = name => resolve(output, name);
const json = (name, value) => writeFile(path(name), JSON.stringify(value, null, 2) + '\n');
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const fixture = await readFile(new URL('./fixtures/css-sticky.html', import.meta.url));
await mkdir(output, { recursive: true });
const report = { fixtureSha256: hash(fixture), ok: false };
const server = http.createServer((request, response) => {
    if (new URL(request.url, 'http://localhost').pathname !== '/') {
        response.writeHead(404);
        response.end();
        return;
    }
    response.setHeader('Content-Type', 'text/html; charset=utf-8');
    response.end(fixture);
});
await new Promise((done, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', done);
});
const origin = 'http://127.0.0.1:' + server.address().port;

async function run(stem, executable, args, timeout = 40_000) {
    await json(stem + '-invocation.json', { executable, args, timeout });
    let result;
    try {
        result = await promisify(execFile)(executable, args, { windowsHide: true, timeout, maxBuffer: 16 << 20 });
    } catch (error) {
        await writeFile(path(stem + '-stderr.txt'), error.stderr || String(error));
        await writeFile(path(stem + '-stdout.json'), error.stdout || '');
        throw new Error(stem + ' failed: ' + (error.stderr || error.message));
    }
    await writeFile(path(stem + '-stdout.json'), result.stdout);
    await writeFile(path(stem + '-stderr.txt'), result.stderr);
    return JSON.parse(result.stdout);
}

function receipt(dom) {
    const pre = dom.match(/<pre\b[^>]*\bid="results"[^>]*>([^<]*)<\/pre>/);
    assert.ok(pre?.[1], 'Scenario did not emit geometry results');
    const result = JSON.parse(pre[1].replaceAll('&quot;', '"').replaceAll('&lt;', '<').replaceAll('&gt;', '>').replaceAll('&amp;', '&'));
    assert.equal(Object.keys(result.checks).length, 14);
    return result;
}

function verifyChecks(result) {
    for (const [name, check] of Object.entries(result.checks)) {
        assert.deepEqual(check.actual, check.expected, name);
    }
}

// RGB/RGBA, 8-bit interleaved, non-interlaced PNG emitted by the Rust painter.
function decodePng(png) {
    assert.equal(png.subarray(0, 8).toString('hex'), '89504e470d0a1a0a');
    const width = png.readUInt32BE(16);
    const height = png.readUInt32BE(20);
    assert.equal(png[24], 8);
    assert.ok([2, 6].includes(png[25]));
    assert.equal(png[28], 0);
    const channelCount = png[25] === 6 ? 4 : 3;
    const chunks = [];
    for (let offset = 8; offset < png.length;) {
        const size = png.readUInt32BE(offset);
        if (png.toString('ascii', offset + 4, offset + 8) === 'IDAT') {
            chunks.push(png.subarray(offset + 8, offset + 8 + size));
        }
        offset += size + 12;
    }
    const encoded = inflateSync(Buffer.concat(chunks));
    const stride = width * channelCount;
    assert.equal(encoded.length, height * (stride + 1));
    const pixels = Buffer.alloc(stride * height);
    const paeth = (left, up, diagonal) => {
        const prediction = left + up - diagonal;
        const a = Math.abs(prediction - left);
        const b = Math.abs(prediction - up);
        const c = Math.abs(prediction - diagonal);
        return a <= b && a <= c ? left : b <= c ? up : diagonal;
    };
    for (let y = 0; y < height; y++) {
        const filter = encoded[y * (stride + 1)];
        assert.ok(filter < 5);
        for (let i = 0; i < stride; i++) {
            const index = y * stride + i;
            const left = i >= channelCount ? pixels[index - channelCount] : 0;
            const up = y > 0 ? pixels[index - stride] : 0;
            const diagonal = i >= channelCount && y > 0 ? pixels[index - stride - channelCount] : 0;
            const decoded = [0, left, up, (left + up) >> 1, paeth(left, up, diagonal)][filter];
            pixels[index] = encoded[index + y + 1] + decoded;
        }
    }
    return { width, height, pixels, channelCount };
}

const colors = [[13, 147, 70], [51, 37, 196]];
function samples(png, points) {
    const { width, height, channelCount, pixels } = decodePng(png);
    return points.map(([x, y]) => {
        assert.ok(x >= 0 && x < width && y >= 0 && y < height);
        const index = (y * width + x) * channelCount;
        return Array.from(pixels.subarray(index, index + 3));
    });
}

function countColors(png) {
    const { pixels, channelCount } = decodePng(png);
    const counts = colors.map(() => 0);
    for (let index = 0; index < pixels.length; index += channelCount) {
        for (const [sample, [r, g, b]] of colors.entries()) {
            if (pixels[index] === r && pixels[index + 1] === g && pixels[index + 2] === b) counts[sample]++;
        }
    }
    return counts;
}

async function rust() {
    report.binary = { path: binary, sha256: hash(await readFile(binary)) };
    const headless = await run('rust-headless', binary, [
        '--headless', '--url', origin, '--width', '800', '--height', '600',
        '--click-id', 'run', '--run-for-ms', '400', '--profile', path('profile-' + randomUUID()),
        '--dump-dom', path('rust-headless.html'), '--screenshot', path('rust-headless.png'),
    ]);
    report.headless = { summary: headless };
    const result = receipt(await readFile(path('rust-headless.html'), 'utf8'));
    report.headless.result = result;
    verifyChecks(result);
    assert.equal(headless.title, 'Sticky verified', JSON.stringify(headless.javascript));
    assert.deepEqual(headless.javascript.diagnostics, []);
    const screenshot = await readFile(path('rust-headless.png'));
    const points = result.paint.map(([x, y]) => [Math.round(x) + 4, Math.round(y) + 4]);
    const actual = samples(screenshot, points);
    assert.deepEqual(actual, colors);
    report.headless.pixels = { points, actual, sha256: hash(screenshot) };
    await json('native-actions.json', [
        { action: 'title', value: 'Sticky ready' },
        { action: 'click', id: 'run' },
        { action: 'title', value: 'Sticky verified' },
    ]);
    const native = await run('rust-native', binary, [
        '--url', origin, '--smoke-actions', path('native-actions.json'),
        '--smoke-window', path('rust-native.png'), '--profile', path('profile-' + randomUUID()),
    ]);
    report.native = { summary: native };
    assert.equal(native.title, 'Sticky verified', JSON.stringify(native.javascript));
    assert.deepEqual(native.javascript.diagnostics, []);
    assert.ok(native.frames_presented > 0);
    const frame = await readFile(path('rust-native.png'));
    const counts = countColors(frame);
    assert.ok(counts.every(count => count > 250), JSON.stringify(counts));
    report.native.pixels = { colors, counts, sha256: hash(frame) };
    assert.equal(hash(await readFile(binary)), report.binary.sha256);
}

async function reference() {
    await json('chrome-scenario.json', {
        browser: { mode: 'launch', executable_path: chrome, headless: true },
        start_url: { url: origin },
        actions: [{
            step_id: 'scroll', action: 'click', timeout_ms: 20_000,
            locator: { kind: 'css', selector: '#run' },
        }, {
            step_id: 'geometry', action: 'wait_for', timeout_ms: 20_000,
            locator: { kind: 'css', selector: '#results[data-complete="yes"]' }, state: 'attached',
        }],
        capture: { at_end: ['url', 'dom'], events: ['page-errors'] },
        command_timeout_ms: 50_000,
    });
    const response = await run('chrome-rea', process.execPath, [
        rea, 'capture-browser-scenario', path('chrome-scenario.json'), '--format', 'json', '--full-output',
    ], 75_000);
    assert.equal(response.ok, true);
    const capture = response.data.normalized_result;
    assert.ok(capture.steps.every(step => step.status === 'completed'));
    assert.equal(capture.browser.process_ownership, 'provider-owned');
    assert.equal(capture.browser.cleanup, 'terminated-owned-process');
    const dom = capture.steps.at(-1).artifacts.dom.value.text;
    await writeFile(path('chrome.html'), dom);
    const result = receipt(dom);
    report.chrome = { result, evidenceId: response.data.evidence_id, browser: capture.browser };
    verifyChecks(result);
    assert.ok(dom.includes('<title>Sticky verified</title>'));
}

try {
    if (!chromeOnly) await rust();
    await reference();
    if (!chromeOnly) {
        assert.deepEqual(report.headless.result.checks, report.chrome.result.checks);
    }
    report.ok = true;
} catch (error) {
    report.error = String(error.stack ?? error);
    process.exitCode = 1;
} finally {
    await json('report.json', report);
    server.closeAllConnections();
    await new Promise(done => server.close(done));
}
console.log(JSON.stringify({
    ok: report.ok, sha256: report.binary?.sha256, evidence: report.chrome?.evidenceId,
    report: path('report.json'), error: report.error,
}));
