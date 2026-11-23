// Compare a common fixture in the browser host and owned Chrome.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { once } from 'node:events';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const usage = 'Usage: node tools/verify-keyed-collections.mjs [BROWSER.exe] [OUTPUT] [--chrome-reference | --chrome-reference-only] [--rea PATH] [--chrome PATH]';
const args = process.argv.slice(2);
const paths = [];
const options = new Map();
for (let index = 0; index < args.length; index++) {
    const argument = args[index];
    if (argument === '--chrome-reference' || argument === '--chrome-reference-only') {
        options.set(argument, true);
    } else if (argument === '--rea' || argument === '--chrome') {
        assert.ok(args[index + 1] && !args[index + 1].startsWith('--'), usage);
        options.set(argument, args[++index]);
    } else {
        assert.ok(!argument.startsWith('--'), usage);
        paths.push(argument);
    }
}
assert.ok(paths.length <= 2, usage);
const chromeOnly = options.has('--chrome-reference-only');
const reference = chromeOnly || options.has('--chrome-reference');
const binary = chromeOnly ? null : resolve(paths[0] ?? resolve(root, 'target/release/rust-browser.exe'));
const output = resolve(paths[1] ?? resolve(root, 'target/keyed-collections-verification'));
const rea = resolve(options.get('--rea') ?? process.env.REA_CLI ?? resolve(root, 'node_modules/rea-agents/scripts/rea.mjs'));
const chrome = resolve(options.get('--chrome') ?? resolve(process.env.ProgramFiles ?? 'C:/Program Files', 'Google/Chrome/Application/chrome.exe'));
await mkdir(output, { recursive: true });
const fixture = await readFile(new URL('./fixtures/keyed-collections/index.html', import.meta.url));
const sha256 = bytes => createHash('sha256').update(bytes).digest('hex');
const save = (name, value) => writeFile(resolve(output, name), JSON.stringify(value, null, 2) + '\n');
const expected = {
    equal: [9, 2, 4, 5, 6, 7, 8, 9, 10, 11, true, true],
    iteration: ['first:1', 'third:33', 'late:4', true, true],
    algebra: ['3,1,2', '2,3', '1', '1', false, true, false],
    visits: '1:1,3:3,4:4',
    iteratorClose: [1, true],
    groups: ['6,8', '7,9'],
    computed: ['computed', 'computed', true],
    weak: [11, 12, 13, true, true, false, false, true, false, 15],
    aliases: [true, true, true],
};
const report = {
    binary, fixtureSha256: sha256(fixture), expected, runs: {}, processes: {}, errors: [],
    runnerSha256: sha256(await readFile(fileURLToPath(import.meta.url))),
};
const server = createServer((request, response) => {
    if (request.url !== '/collections') {
        response.writeHead(404);
        response.end();
        return;
    }
    response.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8', 'Cache-Control': 'no-store' });
    response.end(fixture);
});
server.listen(0, '127.0.0.1');
await once(server, 'listening');
const url = 'http://127.0.0.1:' + server.address().port + '/collections';

async function run(executable, args, stem, timeout) {
    const child = spawn(executable, args, { windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
    const stdout = [], stderr = [];
    child.stdout.on('data', data => stdout.push(data));
    child.stderr.on('data', data => stderr.push(data));
    const receipt = { executable, args, timedOut: false };
    report.processes[stem] = receipt;
    const timer = setTimeout(() => { receipt.timedOut = true; child.kill(); }, timeout);
    try {
        [receipt.exitCode, receipt.signal] = await once(child, 'close');
    } finally {
        clearTimeout(timer);
    }
    const bytes = Buffer.concat(stdout);
    await Promise.all([
        writeFile(resolve(output, stem + '-stdout.json'), bytes),
        writeFile(resolve(output, stem + '-stderr.txt'), Buffer.concat(stderr)),
    ]);
    assert.equal(receipt.timedOut, false, stem + ' exceeded its process deadline');
    assert.equal(receipt.exitCode, 0, Buffer.concat(stderr).toString('utf8').slice(0, 1000));
    return JSON.parse(bytes);
}

function verify(dom) {
    const match = /<pre\b[^>]*\bid="receipt"[^>]*>([^<]*)<\/pre>/.exec(dom);
    assert.ok(match && match[0].includes('data-complete="yes"'), match?.[1] ?? 'Missing test receipt');
    assert.match(dom, /<title>Keyed collections OK<\/title>/);
    const entities = { amp: '&', lt: '<', gt: '>', quot: '"', '#39': "'" };
    const result = JSON.parse(match[1].replace(/&(amp|lt|gt|quot|#39);/g, (_, key) => entities[key]));
    assert.deepEqual(result, expected);
    return result;
}

async function check(name, action) {
    try {
        await action();
    } catch (error) {
        report.errors.push({ name, error: String(error.stack ?? error) });
        console.error(name + ': ' + error.message);
    }
}

try {
    if (binary) await check('Rust browser', async () => {
        const dom = resolve(output, 'rust-final.html');
        const image = resolve(output, 'rust-final.png');
        report.binarySha256 = sha256(await readFile(binary));
        const result = await run(binary, [
            '--headless', '--url', url, '--profile', resolve(output, 'qa-' + randomUUID()),
            '--run-for-ms', '3000', '--dump-dom', dom, '--screenshot', image,
        ], 'rust', 20000);
        report.runs.rust = { result };
        assert.deepEqual(result.javascript.diagnostics, []);
        assert.equal(result.title, 'Keyed collections OK');
        assert.equal(result.layout_limited, false);
        assert.equal(result.paint_limited, false);
        report.runs.rust.receipt = verify(await readFile(dom, 'utf8'));
        report.runs.rust.frameSha256 = sha256(await readFile(image));
    });
    if (reference) await check('Owned Chrome through REA', async () => {
        const scenario = {
            browser: { mode:'launch', executable_path:chrome, headless:true },
            start_url: { url },
            actions: [{ step_id:'collection-receipt', action:'wait_for', timeout_ms:10000,
                locator:{kind:'css',selector:'#receipt[data-complete]'}, state:'attached' }],
            capture: { at_end:['url','dom'], events:['page-errors'] },
            command_timeout_ms:25000,
        };
        await save('chrome-scenario.json', scenario);
        const result = await run(process.execPath, [
            rea, 'capture-browser-scenario', resolve(output, 'chrome-scenario.json'),
            '--format', 'json', '--full-output',
        ], 'chrome-rea', 40000);
        assert.equal(result.ok, true);
        const capture = result.data.normalized_result;
        assert.equal(capture.browser.process_ownership, 'provider-owned');
        assert.equal(capture.browser.cleanup, 'terminated-owned-process');
        const step = capture.steps.at(-1);
        assert.equal(step.status, 'completed');
        assert.equal(step.artifacts.dom.state, 'captured');
        const dom = step.artifacts.dom.value.text;
        await writeFile(resolve(output, 'chrome-final.html'), dom);
        report.runs.chrome = { evidenceId: result.data.evidence_id, browser: capture.browser,
            limitations: result.data.limitations, receipt: verify(dom) };
    });
} finally {
    const closed = once(server, 'close');
    server.close();
    server.closeAllConnections();
    await closed;
    report.ok = report.errors.length === 0;
    await Promise.all([save('report.json', report), writeFile(resolve(output, 'fixture.html'), fixture)]);
}
console.log(JSON.stringify({ ok:report.ok, binarySha256:report.binarySha256,
    chromeEvidence:report.runs.chrome?.evidenceId, report:resolve(output, 'report.json') }));
if (!report.ok) process.exitCode = 1;
