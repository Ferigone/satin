import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { once } from 'node:events';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { resolve } from 'node:path';
import { promisify } from 'node:util';

const [binaryArg = 'target/release/rust-browser.exe', outputArg = 'target/image-bitmap-verification',
    mode = 'both', expectedSha] = process.argv.slice(2);
assert.ok(['both', 'chrome', 'rust'].includes(mode));
const binary = resolve(binaryArg), output = resolve(outputArg);
const rea = process.env.REA_CLI;
const chrome = process.env.CHROME_EXE ?? 'C:/Program Files/Google/Chrome/Application/chrome.exe';
if (mode !== 'rust') assert.ok(rea, 'Set REA_CLI to the path of the REA CLI script');
await mkdir(output, {recursive: true});
const run = promisify(execFile);
const options = {windowsHide: true, timeout: 40000, maxBuffer: 4 * 1024 * 1024};
const sha = bytes => createHash('sha256').update(bytes).digest('hex');
const hash = mode === 'chrome' ? null : sha(await readFile(binary));
if (expectedSha) assert.equal(hash, expectedSha.toLowerCase());
const json = (name, value) => writeFile(resolve(output, name), JSON.stringify(value, null, 2) + '\n');
const fixture = await readFile(new URL('./fixtures/image-bitmap.html', import.meta.url), 'utf8');
const png = Buffer.from(
    'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGNg+M/wHwAEAQH/cetH5QAAAABJRU5ErkJggg==',
    'base64',
);

async function listen(server) {
    server.listen(0, '127.0.0.1');
    await once(server, 'listening');
    return 'http://127.0.0.1:' + server.address().port;
}
function image(response) {
    response.writeHead(200, {'Content-Type': 'image/png', 'Cache-Control': 'no-store'}).end(png);
}
const cross = createServer((request, response) => {
    if (request.url === '/green.png') image(response);
    else response.writeHead(404).end();
});
const otherOrigin = await listen(cross);
const served = fixture.replace('__CROSS__', otherOrigin);
const server = createServer((request, response) => {
    if (request.url === '/') response.writeHead(200, {'Content-Type': 'text/html; charset=utf-8'}).end(served);
    else if (request.url === '/green.png') image(response);
    else response.writeHead(404).end();
});

function receipt(dom) {
    const content = /<pre\b(?=[^>]*\bid="receipt")(?=[^>]*\bdata-complete="yes")[^>]*>([^<]*)<\/pre>/.exec(dom);
    assert.ok(content, 'ImageBitmap receipt is missing');
    const entities = {amp: '&', lt: '<', gt: '>', quot: '"', '#39': "'"};
    return JSON.parse(content[1].replace(/&(amp|lt|gt|quot|#39);/g, (_, entity) => entities[entity]));
}

const report = {browserSha256: hash, fixtureSha256: sha(fixture), pngSha256: sha(png), runs: {}};
const expected = {
    snapshot: [true, true, 6, 4, '0,0,0,0', '0,0,255,255', '255,255,255,255', '255,0,0,255'],
    transfer: [true, 6, 4, 6, [0, 0, 'InvalidStateError']],
    blob: [3, 3, '0,255,0,255'],
    inputErrors: ['InvalidStateError', 'RangeError', 'InvalidStateError', 'TypeError'],
    origin: ['0,255,0,255', 'DataCloneError', 'DataCloneError', 1, 'SecurityError'],
};
try {
    const url = await listen(server);
    if (mode !== 'chrome') {
        const dom = resolve(output, 'rust-dom.html');
        const result = await run(binary, ['--headless', '--url', url, '--run-for-ms', '800',
            '--profile', resolve(output, 'profile-' + randomUUID()), '--dump-dom', dom], options);
        const page = JSON.parse(result.stdout);
        report.runs.rust = {diagnostics: page.javascript.diagnostics, values: receipt(await readFile(dom, 'utf8'))};
        assert.equal(sha(await readFile(binary)), hash);
    }
    if (mode !== 'rust') {
        const scenario = {
            browser: {mode: 'launch', executable_path: chrome, headless: true},
            start_url: {url},
            actions: [{step_id: 'receipt', action: 'wait_for', state: 'attached', timeout_ms: 10000,
                locator: {kind: 'css', selector: '#receipt[data-complete="yes"]'}}],
            capture: {at_end: ['url', 'dom'], events: ['page-errors']},
            command_timeout_ms: 25000,
        };
        await json('chrome-scenario.json', scenario);
        const result = await run(process.execPath, [rea, 'capture-browser-scenario',
            resolve(output, 'chrome-scenario.json'), '--format', 'json', '--full-output'], options);
        const capture = JSON.parse(result.stdout);
        await json('chrome-rea.json', capture);
        assert.ok(capture.ok);
        const normalized = capture.data.normalized_result;
        const step = normalized.steps.at(-1);
        assert.equal(step.status, 'completed');
        assert.equal(normalized.browser.process_ownership, 'provider-owned');
        assert.equal(normalized.browser.cleanup, 'terminated-owned-process');
        report.runs.chrome = {evidenceId: capture.data.evidence_id, browser: normalized.browser,
            values: receipt(step.artifacts.dom.value.text)};
    }
    await json('report.json', report);
    for (const run of Object.values(report.runs)) {
        assert.deepEqual(run.diagnostics ?? [], []);
        assert.deepEqual(run.values, expected);
    }
    console.log(JSON.stringify(report));
} finally {
    await Promise.all([server, cross].map(item => new Promise(done => item.close(done))));
}
