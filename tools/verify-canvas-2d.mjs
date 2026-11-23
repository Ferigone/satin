import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import http from 'node:http';
import { resolve } from 'node:path';
import { inflateSync } from 'node:zlib';

const binary = resolve(process.argv[2] ?? 'target/debug/rust-browser.exe');
const output = resolve(process.argv[3] ?? 'target/canvas-verification');
const argument = name => {
    const index = process.argv.indexOf(name);
    return index < 0 ? undefined : process.argv[index + 1];
};
const rea = argument('--rea') ?? process.env.REA_CLI;
const chrome = argument('--chrome') ?? 'C:/Program Files/Google/Chrome/Application/chrome.exe';
const chromeOnly = process.argv.includes('--chrome-only');
const rustOnly = process.argv.includes('--rust-only');
assert.ok(!(chromeOnly && rustOnly));
if (!rustOnly) assert.ok(rea, 'Supply the REA CLI script via --rea or REA_CLI');
await mkdir(output, { recursive: true });

const green = Buffer.from(
    'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGNg+M/wHwAEAQH/cetH5QAAAABJRU5ErkJggg==',
    'base64',
);
const fixture = await readFile(new URL('./fixtures/canvas-2d.html', import.meta.url), 'utf8');
const fixturesPath = new URL('../crates/engine/tests/fixtures/', import.meta.url);
const wptPath = new URL('wpt-canvas/', fixturesPath);
const wptManifest = JSON.parse(await readFile(new URL('manifest.json', wptPath), 'utf8'));
const [testharness, canvasBridge] = await Promise.all([
    readFile(new URL('wpt-url/bridge.js', fixturesPath)),
    readFile(new URL('bridge.js', wptPath)),
]);
const wptFiles = new Map(await Promise.all(wptManifest.files.map(async ({ file, sha256 }) => {
    const original = await readFile(new URL(file, wptPath));
    assert.equal(createHash('sha256').update(original).digest('hex'), sha256, file);
    return [file, original];
})));
const wptPage = '<!doctype html><meta charset="utf-8"><pre id="results"></pre><script>' + `
const files = ${JSON.stringify([...wptFiles.keys()])};
Promise.all(files.map(file => new Promise(resolve => {
    const iframe = document.createElement('iframe');
    iframe.hidden = true;
    iframe.onload = function() {
        if (iframe.contentWindow.runCanvasWpt) iframe.contentWindow.runCanvasWpt();
        resolve([file, iframe.contentWindow.canvasWptResults || []]);
        iframe.remove();
    };
    iframe.src = '/wpt/' + file;
    document.body.appendChild(iframe);
}))).then(results => {
    const target = document.getElementById('results');
    target.textContent = JSON.stringify(Object.fromEntries(results));
    target.setAttribute('data-complete', 'yes');
});
</script>`;
const requests = [];
const report = { binary: chromeOnly ? null : binary, requests, cases: {}, ok: false };
const path = name => resolve(output, name);
const json = (name, value) => writeFile(path(name), JSON.stringify(value, null, 2) + '\n');
const sha = bytes => createHash('sha256').update(bytes).digest('hex');

function listen(server) {
    return new Promise((ready, reject) => {
        server.once('error', reject);
        server.listen(0, '127.0.0.1', () => ready('http://127.0.0.1:' + server.address().port));
    });
}
function redirect(res, location) {
    res.writeHead(302, { location });
    res.end();
}
const foreign = http.createServer((req, res) => {
    requests.push({ host: 'other origin', method: req.method, path: req.url, origin: req.headers.origin });
    if (req.url === '/back') return redirect(res, origin + '/green.png');
    res.setHeader('Content-Type', 'image/png');
    if (req.url === '/green.png') res.setHeader('Access-Control-Allow-Origin', '*');
    res.end(green);
});
const cross = await listen(foreign);
const source = fixture.replaceAll('__CROSS__', cross);
const ready = new Set();
const holds = new Map();
const same = http.createServer((req, res) => {
    requests.push({ host: 'document origin', method: req.method, path: req.url, origin: req.headers.origin });
    const requestUrl = new URL(req.url, 'http://localhost');
    const route = requestUrl.pathname;
    const mode = requestUrl.searchParams.get('run');
    switch (route) {
        case '/':
            res.setHeader('Content-Type', 'text/html; charset=utf-8');
            res.end(source);
            break;
        case '/wpt':
            res.setHeader('Content-Type', 'text/html; charset=utf-8');
            res.end(wptPage);
            break;
        case '/resources/testharness.js':
        case '/html/canvas/resources/canvas-tests.js':
            res.setHeader('Content-Type', 'text/javascript; charset=utf-8');
            res.end(route === '/resources/testharness.js' ? testharness : canvasBridge);
            break;
        case '/resources/testharnessreport.js':
        case '/html/canvas/resources/canvas-tests.css':
            res.end();
            break;
        case '/green.png':
        case '/bad.png':
            res.setHeader('Content-Type', 'image/png');
            res.end(route === '/green.png' ? green : 'broken image');
            break;
        case '/cross-redirect':
            redirect(res, cross + '/green.png');
            break;
        case '/cross-bounce':
            redirect(res, cross + '/back');
            break;
        case '/fixture-hold':
            if (ready.has(mode)) res.end();
            else holds.set(mode, res);
            break;
        case '/fixture-ready':
            ready.add(mode);
            holds.get(mode)?.end();
            holds.delete(mode);
            res.end();
            break;
        default:
            if (wptFiles.has(route.slice(5)) && route.startsWith('/wpt/')) {
                res.setHeader('Content-Type', 'text/html; charset=utf-8');
                res.end(wptFiles.get(route.slice(5)));
            } else {
                res.writeHead(404);
                res.end();
            }
    }
});
const origin = await listen(same);

async function command(executable, args, stem, timeout = 40_000) {
    await json(stem + '-invocation.json', { executable, args, timeout });
    const run = await new Promise((done, reject) => {
        const child = spawn(executable, args, { windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
        const stdout = [];
        const stderr = [];
        let expired = false;
        const timer = setTimeout(() => {
            expired = true;
            child.kill();
        }, timeout);
        child.stdout.on('data', chunk => stdout.push(chunk));
        child.stderr.on('data', chunk => stderr.push(chunk));
        child.once('error', error => {
            clearTimeout(timer);
            reject(error);
        });
        child.once('close', code => {
            clearTimeout(timer);
            done({
                code, expired, stdout: Buffer.concat(stdout).toString('utf8'),
                stderr: Buffer.concat(stderr).toString('utf8'),
            });
        });
    });
    await Promise.all([
        writeFile(path(stem + '-stdout.json'), run.stdout),
        writeFile(path(stem + '-stderr.txt'), run.stderr),
    ]);
    assert.equal(run.expired, false, stem + ' timeout');
    assert.equal(run.code, 0, stem + '\n' + run.stderr);
    return JSON.parse(run.stdout);
}

function domResults(dom) {
    const html = dom.match(/<pre\b[^>]*\bid="results"[^>]*>([^<]*)<\/pre>/);
    assert.ok(html, 'Canvas result element missing');
    return JSON.parse(html[1].replaceAll('&lt;', '<').replaceAll('&gt;', '>').replaceAll('&amp;', '&'));
}

// Rust's screenshots are ordinary 8-bit noninterlaced RGB or RGBA PNGs.
function screenshot(png) {
    const width = png.readUInt32BE(16);
    const height = png.readUInt32BE(20);
    const channels = png[25] === 6 ? 4 : 3;
    assert.equal(png[24], 8);
    assert.ok([2, 6].includes(png[25]));
    assert.equal(png[28], 0);
    const chunks = [];
    for (let start = 8; start < png.length;) {
        const size = png.readUInt32BE(start);
        if (png.toString('ascii', start + 4, start + 8) === 'IDAT') {
            chunks.push(png.subarray(start + 8, start + 8 + size));
        }
        start += size + 12;
    }
    const encoded = inflateSync(Buffer.concat(chunks));
    const stride = width * channels;
    assert.equal(encoded.length, height * (stride + 1));
    const rgb = Buffer.alloc(stride * height);
    const paeth = (a, b, c) => {
        const p = a + b - c;
        const [da, db, dc] = [Math.abs(p - a), Math.abs(p - b), Math.abs(p - c)];
        return da <= db && da <= dc ? a : db <= dc ? b : c;
    };
    for (let y = 0; y < height; y++) {
        const filter = encoded[y * (stride + 1)];
        assert.ok(filter <= 4);
        for (let x = 0; x < stride; x++) {
            const index = y * stride + x;
            const a = x >= channels ? rgb[index - channels] : 0;
            const b = y > 0 ? rgb[index - stride] : 0;
            const c = x >= channels && y > 0 ? rgb[index - stride - channels] : 0;
            const predict = [0, a, b, (a + b) >> 1, paeth(a, b, c)][filter];
            rgb[index] = encoded[index + y + 1] + predict;
        }
    }
    return {
        pixel(x, y) {
            const index = (y * width + x) * channels;
            return Array.from(rgb.subarray(index, index + 3));
        },
        count([r, g, b]) {
            let count = 0;
            // The canvas begins at the left edge of the document in both paths.
            for (let y = 0; y < height; y++) {
                for (let x = 0; x < Math.min(width, 200); x++) {
                    const index = (y * width + x) * channels;
                    if (rgb[index] === r && rgb[index + 1] === g && rgb[index + 2] === b) count++;
                }
            }
            return count;
        },
    };
}

async function verifyRust() {
    const png = path('rust-headless.png');
    const dom = path('rust-headless.html');
    const headless = await command(binary, [
        '--headless', '--url', origin + '/?run=headless',
        '--width', '480', '--height', '260', '--run-for-ms', '1200',
        '--profile', path('qa-' + randomUUID()), '--dump-dom', dom, '--screenshot', png,
    ], 'rust-headless');
    report.cases.headless = { summary: headless };
    const result = domResults(await readFile(dom, 'utf8'));
    Object.assign(report.cases.headless, result);
    assert.equal(Object.keys(result.checks).length, 32, JSON.stringify(headless.javascript));
    assert.equal(headless.title, 'Canvas 2D OK', JSON.stringify(result));
    assert.deepEqual(headless.javascript.diagnostics, []);
    assert.equal(headless.layout_limited, false);
    assert.equal(headless.paint_limited, false);
    const frame = screenshot(await readFile(png));
    assert.deepEqual(frame.pixel(20, 20), [0, 255, 0]);
    assert.deepEqual(frame.pixel(60, 20), [255, 255, 0]);
    report.cases.headless.documentSamples = [frame.pixel(20, 20), frame.pixel(60, 20)];

    await json('native-actions.json', [{ action: 'title', value: 'Canvas 2D OK' }]);
    const nativePath = path('rust-native.png');
    const native = await command(binary, [
        '--url', origin + '/?run=native', '--profile', path('qa-' + randomUUID()),
        '--smoke-actions', path('native-actions.json'), '--smoke-window', nativePath,
    ], 'rust-native');
    report.cases.native = { summary: native };
    assert.equal(native.title, 'Canvas 2D OK');
    assert.ok(native.frames_presented >= 1);
    assert.deepEqual(native.javascript.diagnostics, []);
    assert.equal(native.layout_limited, false);
    assert.equal(native.paint_limited, false);
    const nativeFrame = screenshot(await readFile(nativePath));
    const greenPixels = nativeFrame.count([0, 255, 0]);
    const yellowPixels = nativeFrame.count([255, 255, 0]);
    assert.ok(greenPixels >= 100 && yellowPixels >= 100, 'Painted and unpainted canvas areas should be visible');
    report.cases.native.documentColors = { greenPixels, yellowPixels };
}

async function captureChrome(stem, url) {
    await json(stem + '-scenario.json', {
        browser: { mode: 'launch', executable_path: chrome, headless: true },
        start_url: { url },
        actions: [{
            step_id: 'canvas-results', action: 'wait_for', timeout_ms: 25_000,
            locator: { kind: 'css', selector: '#results[data-complete="yes"]' }, state: 'attached',
        }],
        capture: { at_end: ['url', 'dom'], events: ['page-errors', 'network'] },
        command_timeout_ms: 60_000,
    });
    const response = await command(process.execPath, [
        rea, 'capture-browser-scenario', path(stem + '-scenario.json'), '--format', 'json', '--full-output',
    ], stem + '-rea', 90_000);
    const capture = response.data?.normalized_result;
    assert.equal(response.ok, true);
    assert.equal(capture.browser.process_ownership, 'provider-owned');
    assert.equal(capture.browser.cleanup, 'terminated-owned-process');
    assert.ok(capture.steps.every(step => step.status === 'completed'));
    const dom = capture.steps.at(-1).artifacts.dom.value.text;
    await writeFile(path(stem + '.html'), dom);
    return { evidenceId: response.data.evidence_id, browser: capture.browser, results: domResults(dom) };
}

async function verifyChrome() {
    const { evidenceId, browser, results } = await captureChrome('chrome', origin + '/?run=chrome');
    const { checks, fault } = results;
    const differences = {};
    report.cases.chrome = { evidenceId, browser, checks, fault, differences };
    assert.equal(fault, null);
    assert.equal(Object.keys(checks).length, 32);
    for (const [name, check] of Object.entries(checks)) {
        if (check.pass) continue;
        assert.equal(name, 'opaque canvas', JSON.stringify(check));
        assert.deepEqual(check.actual, [false, [0, 0, 0, 0]]);
        differences[name] = check;
    }
    const reference = await captureChrome('chrome-wpt', origin + '/wpt');
    report.cases.chromeWpt = { ...reference, revision: wptManifest.revision };
    assert.equal(Object.keys(reference.results).length, wptFiles.size);
    for (const { file, subtests } of wptManifest.files) {
        const tests = reference.results[file];
        assert.equal(tests.length, subtests, file);
        for (const test of tests) {
            assert.equal(test.status, 'PASS', file + ': ' + JSON.stringify(test));
        }
    }
}

try {
    await writeFile(path('fixture.html'), source);
    if (!chromeOnly) {
        report.binarySha256 = sha(await readFile(binary));
        await verifyRust();
        assert.equal(sha(await readFile(binary)), report.binarySha256);
    }
    if (!rustOnly) await verifyChrome();
    if (report.cases.headless && report.cases.chrome) {
        const differences = new Set(Object.keys(report.cases.chrome.differences));
        const compared = checks => Object.fromEntries(Object.entries(checks).filter(([name]) => !differences.has(name)));
        assert.deepEqual(compared(report.cases.headless.checks), compared(report.cases.chrome.checks));
    }
    report.ok = true;
} catch (error) {
    report.error = String(error.stack ?? error);
    process.exitCode = 1;
} finally {
    await json('report.json', report);
    for (const server of [same, foreign]) {
        server.closeAllConnections();
        await new Promise(done => server.close(done));
    }
}
process.stdout.write(JSON.stringify({
    ok: report.ok, binarySha256: report.binarySha256, evidence: report.cases.chrome?.evidenceId,
    report: path('report.json'), error: report.error,
}) + '\n');
