import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { createHash } from 'node:crypto';
import { once } from 'node:events';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { resolve } from 'node:path';
import { promisify } from 'node:util';

const [outputArg, rea = process.env.REA_CLI, rustBinary] = process.argv.slice(2);
assert.ok(outputArg && rea, 'Usage: node tools/verify-css-calc.mjs OUTPUT REA_CLI [RUST_BROWSER_EXE]');
const output = resolve(outputArg);
await mkdir(output, { recursive: true });
const save = (name, value) => writeFile(resolve(output, name), JSON.stringify(value, null, 2) + '\n');
const sha256 = bytes => createHash('sha256').update(bytes).digest('hex');
const run = promisify(execFile);
const fixture = await readFile(new URL('./fixtures/css-calc.html', import.meta.url));
const harness = String.raw`<!doctype html><meta charset="utf-8"><title>CSS calc() verification</title>
<style>body{margin:0}iframe{display:block;border:0;width:800px;height:600px}</style>
<pre id="calc-summary"></pre><script>
const frame = document.createElement('iframe');
frame.src = '/css-calc.html';
frame.onload = async () => {
    const child = frame.contentWindow;
    const originalDocument = child.document;
    const originalElement = child.viewportBox;
    const snapshots = [];
    const take = () => snapshots.push(child.calcSnapshot());
    async function resize(width, height) {
        await new Promise((resolve, reject) => {
            const timeout = setTimeout(() => reject(new Error('No resize to ' + width + 'x' + height)), 5000);
            function resized() {
                if (child.innerWidth !== width || child.innerHeight !== height) return;
                clearTimeout(timeout);
                child.removeEventListener('resize', resized);
                resolve();
            }
            child.addEventListener('resize', resized);
            frame.style.width = width + 'px';
            frame.style.height = height + 'px';
        });
        take();
    }
    const summary = document.getElementById('calc-summary');
    try {
        take();
        await resize(800, 430);
        await resize(620, 430);
        await resize(800, 600);
        child.changeCalcContainer();
        take();
        child.changeCalcRootFont();
        take();
        summary.textContent = JSON.stringify({
            snapshots,
            sameDocument: child.document === originalDocument && child.viewportBox === originalElement,
        });
    } catch (error) {
        summary.textContent = JSON.stringify({ error: String(error), snapshots });
    }
    summary.dataset.ready = 'yes';
};
document.body.append(frame);
</script>`;

function receipt(dom, id) {
    const match = new RegExp('<pre\\b(?=[^>]*\\bid="' + id + '")(?=[^>]*\\bdata-ready="yes")[^>]*>([^<]*)<\\/pre>').exec(dom);
    assert.ok(match, `Missing ${id} receipt`);
    const entities = { amp: '&', lt: '<', gt: '>', quot: '"', '#39': "'" };
    return JSON.parse(match[1].replace(/&(amp|lt|gt|quot|#39);/g, (_, key) => entities[key]));
}

function equivalent(observed, expected, label) {
    if (typeof expected === 'number') {
        assert.ok(typeof observed === 'number' && Math.abs(observed - expected) < 0.02,
            `${label}: expected ${expected}, got ${observed}`);
    } else if (expected !== null && typeof expected === 'object') {
        assert.ok(observed !== null && typeof observed === 'object');
        assert.deepEqual(Object.keys(observed).sort(), Object.keys(expected).sort(), label);
        for (const key of Object.keys(expected)) equivalent(observed[key], expected[key], `${label}.${key}`);
    } else {
        assert.equal(observed, expected, label);
    }
}

function expected(width, height, changedContainer = false, changedFont = false) {
    const widthPx = width * 0.5 + 20;
    const heightPx = height + 100;
    return {
        viewport: [width, height],
        viewportRect: [widthPx, heightPx],
        viewportStyle: [`${widthPx}px`, `${heightPx}px`],
        initialViewportRect: [420, 700],
        parentRect: changedContainer ? [480, 320] : [320, 240],
        percentRect: changedContainer ? [310, 118] : [214, 90],
        percentStyle: changedContainer ? ['260px', '90px', '14px', '25px'] : ['180px', '70px', '10px', '17px'],
        math: [changedFont ? 57 : 58, 192],
        font: changedFont ? [50, '20px', '23px'] : [54, '22px', '25px'],
        scalar: [10, 10.5],
        negative: [0, 0, -7, '0px'],
        constraints: [52, 15],
        indefinite: [14, 17],
        invalid: [73, 73, 73, 73, 73, 73, 73, 73, 11],
    };
}

const server = createServer((req, res) => {
    const path = new URL(req.url, 'http://localhost').pathname;
    if (path !== '/' && path !== '/css-calc.html') {
        res.writeHead(path === '/favicon.ico' ? 204 : 404).end();
        return;
    }
    res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8', 'Cache-Control': 'no-store' });
    res.end(path === '/' ? harness : fixture);
});
server.listen(0, '127.0.0.1');
await once(server, 'listening');
const origin = `http://127.0.0.1:${server.address().port}`;

try {
    const chrome = process.env.CHROME_EXE ?? 'C:/Program Files/Google/Chrome/Application/chrome.exe';
    const chromeSha256 = sha256(await readFile(chrome));
    const scenario = 'css-calc-chrome-scenario.json';
    await save(scenario, {
        browser: { mode: 'launch', executable_path: chrome, headless: true },
        environment: { viewport: { width: 1000, height: 760, device_scale_factor: 1 } },
        start_url: { url: origin + '/' },
        actions: [{
            step_id: 'css-calc-verification', action: 'wait_for', state: 'attached', timeout_ms: 25000,
            locator: { kind: 'css', selector: '#calc-summary[data-ready="yes"]' },
        }],
        capture: { at_end: ['url', 'dom'], events: ['page-errors'] },
        command_timeout_ms: 50000,
    });
    const { stdout } = await run(process.execPath, [
        rea, 'capture-browser-scenario', resolve(output, scenario), '--format', 'json', '--full-output',
    ], { windowsHide: true, timeout: 70000, maxBuffer: 10 * 1024 * 1024 });
    const response = JSON.parse(stdout);
    await save('css-calc-chrome-rea.json', response);
    assert.equal(response.ok, true);
    const capture = response.data.normalized_result;
    assert.equal(capture.browser.process_ownership, 'provider-owned');
    assert.equal(capture.browser.cleanup, 'terminated-owned-process');
    assert.equal(sha256(await readFile(chrome)), chromeSha256);
    const step = capture.steps.at(-1);
    assert.equal(step.status, 'completed');
    const result = receipt(step.artifacts.dom.value.text, 'calc-summary');
    const report = {
        fixtureSha256: sha256(fixture),
        chromeSha256,
        evidenceId: response.data.evidence_id,
        browser: capture.browser,
        result,
    };
    await save('css-calc-chrome.json', report);
    assert.equal(result.error, undefined);
    assert.equal(result.sameDocument, true);
    const sizes = [[800, 600], [800, 430], [620, 430]];
    equivalent(result.snapshots, [
        ...sizes.map(size => expected(...size)), expected(800, 600),
        expected(800, 600, true), expected(800, 600, true, true),
    ], 'Chrome snapshots');

    if (rustBinary) {
        const browser = resolve(rustBinary);
        const rustSha256 = sha256(await readFile(browser));
        const runs = [];
        for (const [index, [width, height]] of sizes.entries()) {
            const dom = resolve(output, `css-calc-rust-${width}x${height}.html`);
            const { stdout } = await run(browser, [
                '--headless', '--url', origin + '/css-calc.html', '--width', String(width),
                '--height', String(height), '--dump-dom', dom,
            ], { windowsHide: true, timeout: 20000, maxBuffer: 2 * 1024 * 1024 });
            const metrics = JSON.parse(stdout);
            const measured = receipt(await readFile(dom, 'utf8'), 'calc-receipt');
            runs.push({ metrics, measured });
            await save('css-calc-rust.json', { rustSha256, runs });
            assert.deepEqual(metrics.javascript.diagnostics, []);
            const reference = result.snapshots[index];
            // The CLI opens the fixture anew at each size. In Chrome the same
            // captured rectangle is retained across real iframe resize events.
            equivalent(measured, { ...reference, initialViewportRect: reference.viewportRect }, `Rust ${width}x${height}`);
        }
        assert.equal(sha256(await readFile(browser)), rustSha256);
    }
    console.log(JSON.stringify({
        chrome: capture.browser.version, evidenceId: response.data.evidence_id,
        chromeSnapshots: result.snapshots.length, rustComparisons: rustBinary ? sizes.length : 0,
    }));
} finally {
    server.closeAllConnections();
    await new Promise(done => server.close(done));
}
