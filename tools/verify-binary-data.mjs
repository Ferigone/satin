import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { once } from 'node:events';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';

const knownFlags = new Set(['--chrome-reference', '--chrome-reference-only']);
const flags = new Set(process.argv.slice(2).filter(arg => arg.startsWith('--')));
assert.ok([...flags].every(flag => knownFlags.has(flag)));
const [binaryArg = 'target/release/rust-browser.exe', outputArg = 'target/binary-data-verification'] =
    process.argv.slice(2).filter(arg => !arg.startsWith('--'));
const chromeOnly = flags.has('--chrome-reference-only');
const reference = chromeOnly || flags.has('--chrome-reference');
const binary = resolve(binaryArg);
const output = resolve(outputArg);
const rea = process.env.REA_CLI ??
    'C:/Users/admin/AppData/Local/npm-cache/_npx/da7caeae9db9c589/node_modules/rea-agents/scripts/rea.mjs';
const chrome = process.env.CHROME_EXE ?? 'C:/Program Files/Google/Chrome/Application/chrome.exe';
const run = promisify(execFile);
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const fixture = await readFile(new URL('./fixtures/binary-data.html', import.meta.url));
const expected = {
    words: ['0,18,52,33,67,101,135,0', 4660, 2271560481, 1.0009765625],
    aliasing: ['1,1,5,3', '1,1,5,3', '8,8,8', true, false],
    resize: [[1, 0, 2050], true, 0, false, '2050,0'],
    encoded: ['80,196,133,240,159,152,128,239,191,189', 3, 5, '90,90,65,240,159,152,128,90'],
    decoded: ['utf-8', '::😀', '€', true],
    methods: '4,6,10',
};
const report = { fixtureSha256: hash(fixture),
    runnerSha256: hash(await readFile(fileURLToPath(import.meta.url))), expected, runs: {}, errors: [] };
await mkdir(output, { recursive: true });
const save = (name, content) => writeFile(resolve(output, name), JSON.stringify(content, null, 2) + '\n');

const server = createServer((request, response) => {
    if (request.url !== '/binary') {
        response.writeHead(404);
        response.end();
        return;
    }
    response.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8', 'Cache-Control': 'no-store' });
    response.end(fixture);
});
server.listen(0, '127.0.0.1');
await once(server, 'listening');
const url = 'http://127.0.0.1:' + server.address().port + '/binary';

function verify(dom) {
    const match = /<pre\b[^>]*\bid="receipt"[^>]*>([^<]*)<\/pre>/.exec(dom);
    assert.ok(match?.[0].includes('data-complete="yes"'), match?.[1] ?? 'Missing byte data receipt');
    assert.match(dom, /<title>Binary data OK<\/title>/);
    const receipt = JSON.parse(match[1]);
    assert.deepEqual(receipt, expected);
    return receipt;
}

async function compare(name, operation) {
    try {
        report.runs[name] = await operation();
    } catch (error) {
        report.errors.push({ name, message: String(error.stack ?? error) });
        console.error(name + ': ' + error.message);
    }
}

try {
    if (!chromeOnly) await compare('rust', async () => {
        const domPath = resolve(output, 'rust-final.html');
        const framePath = resolve(output, 'rust-final.png');
        report.browserSha256 = hash(await readFile(binary));
        const { stdout } = await run(binary, [
            '--headless', '--url', url, '--profile', resolve(output, 'qa-' + randomUUID()),
            '--run-for-ms', '3000', '--dump-dom', domPath, '--screenshot', framePath,
        ], { windowsHide: true, timeout: 20000, maxBuffer: 8 * 1024 * 1024 });
        const result = JSON.parse(stdout);
        assert.deepEqual(result.javascript.diagnostics, []);
        assert.equal(result.title, 'Binary data OK');
        assert.equal(result.layout_limited, false);
        assert.equal(result.paint_limited, false);
        return { exitCode: 0, frameSha256: hash(await readFile(framePath)),
            receipt: verify(await readFile(domPath, 'utf8')) };
    });
    if (reference) await compare('chrome', async () => {
        const scenario = {
            browser: { mode: 'launch', executable_path: chrome, headless: true },
            start_url: { url },
            actions: [{ step_id: 'binary-receipt', action: 'wait_for', timeout_ms: 10000,
                locator: { kind: 'css', selector: '#receipt[data-complete]' }, state: 'attached' }],
            capture: { at_end: ['url', 'dom'], events: ['page-errors'] },
            command_timeout_ms: 25000,
        };
        await save('chrome-scenario.json', scenario);
        const { stdout } = await run(process.execPath, [
            rea, 'capture-browser-scenario', resolve(output, 'chrome-scenario.json'), '--format', 'json', '--full-output',
        ], { windowsHide: true, timeout: 40000, maxBuffer: 8 * 1024 * 1024 });
        const result = JSON.parse(stdout);
        await save('chrome-rea.json', result);
        assert.equal(result.ok, true);
        const capture = result.data.normalized_result;
        assert.equal(capture.browser.process_ownership, 'provider-owned');
        assert.equal(capture.browser.cleanup, 'terminated-owned-process');
        assert.equal(capture.completeness.status, 'complete');
        assert.equal(capture.events.dropped, 0);
        assert.deepEqual(capture.events.items, []);
        const step = capture.steps.at(-1);
        assert.equal(step.status, 'completed');
        const dom = step.artifacts.dom.value.text;
        await writeFile(resolve(output, 'chrome-final.html'), dom);
        return { exitCode: 0, evidenceId: result.data.evidence_id, browser: capture.browser,
            limitations: result.data.limitations, receipt: verify(dom) };
    });
} finally {
    const closed = once(server, 'close');
    server.close();
    server.closeAllConnections();
    await closed;
    report.ok = report.errors.length === 0;
    await save('report.json', report);
}
console.log(JSON.stringify({ ok: report.ok, browserSha256: report.browserSha256,
    chromeEvidence: report.runs.chrome?.evidenceId, report: resolve(output, 'report.json') }));
if (!report.ok) process.exitCode = 1;
