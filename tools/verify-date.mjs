import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { once } from 'node:events';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';

const argv = process.argv.slice(2);
const flags = new Set(argv.filter(arg => arg.startsWith('--')));
const paths = argv.filter(arg => !arg.startsWith('--'));
assert.ok(paths.length <= 2 && [...flags].every(flag =>
    ['--chrome-reference', '--chrome-reference-only'].includes(flag)),
    'Usage: node tools/verify-date.mjs [BROWSER.exe] [OUTPUT] [--chrome-reference | --chrome-reference-only]');
const [binaryArg = 'target/release/rust-browser.exe', outputArg = 'target/date-verification'] = paths;
const chromeOnly = flags.has('--chrome-reference-only');
const reference = chromeOnly || flags.has('--chrome-reference');
const binary = resolve(binaryArg);
const output = resolve(outputArg);
const rea = process.env.REA_CLI ??
    'C:/Users/admin/AppData/Local/npm-cache/_npx/da7caeae9db9c589/node_modules/rea-agents/scripts/rea.mjs';
const chrome = process.env.CHROME_EXE ?? 'C:/Program Files/Google/Chrome/Application/chrome.exe';
const timezone = process.env.DATE_REFERENCE_TIMEZONE ?? Intl.DateTimeFormat().resolvedOptions().timeZone;
const fixture = await readFile(new URL('./fixtures/date-runtime.html', import.meta.url));
const run = promisify(execFile);
const sha = bytes => createHash('sha256').update(bytes).digest('hex');
const runOptions = { windowsHide: true, maxBuffer: 8 * 1024 * 1024 };
const expected = {
    iso: ['2023-07-02T02:26:15.902Z', '-271821-04-20T00:00:00.000Z', '+275760-09-13T00:00:00.000Z'],
    setters: ['2001-03-01T12:34:56.000Z', '0009-01-01T00:00:00.000Z', 946684800000],
    strings: ['Tue, 29 Feb 2000 12:34:56 GMT', true, true, '"1970-01-01T00:00:00.000Z"'],
    invalid: ['Invalid Date', null, true],
    brand: ['string', 0, '[object Date]', 123, 'custom', 8],
};
const report = {
    fixtureSha256: sha(fixture), runnerSha256: sha(await readFile(fileURLToPath(import.meta.url))),
    referenceTimezone: reference ? timezone : null, runs: {}, errors: [],
};
await mkdir(output, { recursive: true });
const save = (name, content) => writeFile(resolve(output, name), JSON.stringify(content, null, 2) + '\n');

const server = createServer((request, response) => {
    if (request.url !== '/date') {
        response.writeHead(404).end();
        return;
    }
    response.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8', 'Cache-Control': 'no-store' });
    response.end(fixture);
});
server.listen(0, '127.0.0.1');
await once(server, 'listening');
const url = 'http://127.0.0.1:' + server.address().port + '/date';

function verify(dom) {
    assert.match(dom, /<title>Date runtime OK<\/title>/);
    const match = /<pre\b[^>]*\bid="receipt"[^>]*>([^<]*)<\/pre>/.exec(dom);
    assert.ok(match?.[0].includes('data-complete="yes"'), match?.[1] ?? 'Missing Date receipt');
    const escapes = { quot: '"', amp: '&', lt: '<', gt: '>', '#39': "'" };
    const receipt = JSON.parse(match[1].replace(/&(quot|amp|lt|gt|#39);/g, (_, name) => escapes[name]));
    const { local, ...invariants } = receipt;
    assert.deepEqual(invariants, expected);
    assert.equal(local.length, 7);
    assert.ok(local.slice(0, 4).every(Number.isInteger));
    assert.ok(local.slice(4, 6).every(time => /^2024-\d\d-\d\dT\d\d:\d\d:00\.000Z$/.test(time)));
    assert.equal(local[6], true);
    return receipt;
}

async function capture(name, operation) {
    try {
        report.runs[name] = await operation();
    } catch (error) {
        report.errors.push({ name, message: String(error.stack ?? error) });
        console.error(name + ': ' + error.message);
    }
}

try {
    if (!chromeOnly) await capture('rust', async () => {
        const domPath = resolve(output, 'rust-dom.html');
        const framePath = resolve(output, 'rust-frame.png');
        report.browserSha256 = sha(await readFile(binary));
        const { stdout } = await run(binary, [
            '--headless', '--url', url, '--profile', resolve(output, 'qa-' + randomUUID()),
            '--run-for-ms', '2000', '--dump-dom', domPath, '--screenshot', framePath,
        ], { ...runOptions, timeout: 20000 });
        const browser = JSON.parse(stdout);
        assert.deepEqual(browser.javascript.diagnostics, []);
        assert.equal(browser.title, 'Date runtime OK');
        assert.equal(browser.layout_limited, false);
        assert.equal(browser.paint_limited, false);
        return { exitCode: 0, screenshotSha256: sha(await readFile(framePath)),
            receipt: verify(await readFile(domPath, 'utf8')) };
    });
    if (reference) await capture('chrome', async () => {
        const scenario = {
            browser: { mode: 'launch', executable_path: chrome, headless: true },
            start_url: { url },
            environment: { timezone },
            actions: [{ step_id: 'date-receipt', action: 'wait_for', timeout_ms: 10000,
                locator: { kind: 'css', selector: '#receipt[data-complete]', }, state: 'attached' }],
            capture: { at_end: ['url', 'dom'], events: ['page-errors'] },
            command_timeout_ms: 25000,
        };
        await save('chrome-scenario.json', scenario);
        const { stdout } = await run(process.execPath, [
            rea, 'capture-browser-scenario', resolve(output, 'chrome-scenario.json'),
            '--format', 'json', '--full-output',
        ], { ...runOptions, timeout: 40000 });
        const result = JSON.parse(stdout);
        await save('chrome-rea.json', result);
        assert.equal(result.ok, true);
        const observed = result.data.normalized_result;
        assert.equal(observed.browser.process_ownership, 'provider-owned');
        assert.equal(observed.browser.cleanup, 'terminated-owned-process');
        assert.equal(observed.completeness.status, 'complete');
        assert.equal(observed.events.dropped, 0);
        assert.deepEqual(observed.events.items, []);
        const step = observed.steps.at(-1);
        assert.equal(step.status, 'completed');
        const dom = step.artifacts.dom.value.text;
        await writeFile(resolve(output, 'chrome-dom.html'), dom);
        return { exitCode: 0, evidenceId: result.data.evidence_id, browser: observed.browser,
            limitations: result.data.limitations, receipt: verify(dom) };
    });
    if (report.runs.rust && report.runs.chrome) {
        await capture('equality', async () => {
            assert.deepEqual(report.runs.rust.receipt, report.runs.chrome.receipt);
            return { equal: true };
        });
    }
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
