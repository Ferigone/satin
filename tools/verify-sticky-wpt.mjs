import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { createHash } from 'node:crypto';
import { once } from 'node:events';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { resolve } from 'node:path';
import { promisify } from 'node:util';

const [outputArg, rea = process.env.REA_CLI] = process.argv.slice(2);
assert.ok(outputArg && rea, 'Usage: node tools/verify-sticky-wpt.mjs OUTPUT [REA_CLI]');
const output = resolve(outputArg);
await mkdir(output, { recursive: true });

const fixtures = new URL('../crates/engine/tests/fixtures/wpt-sticky/', import.meta.url);
const manifest = JSON.parse(await readFile(new URL('manifest.json', fixtures), 'utf8'));
const routes = new Map();
const js = 'text/javascript; charset=utf-8';
const html = 'text/html; charset=utf-8';

for (const source of [...manifest.files, ...manifest.support]) {
    const bytes = await readFile(new URL('upstream/' + source.file, fixtures));
    assert.equal(createHash('sha256').update(bytes).digest('hex'), source.sha256, source.upstream);
    routes.set('/' + source.upstream, { body: bytes, type: source.file.endsWith('.html') ? html : js });
}

const paths = manifest.files.map(file => '/' + file.upstream);
routes.set('/resources/testharnessreport.js', { type: js, body: String.raw`
const statuses = ['PASS', 'FAIL', 'TIMEOUT', 'NOTRUN', 'PRECONDITION_FAILED'];
setup({ output: false });
add_completion_callback((tests, status) => {
    parent.postMessage({
        path: location.pathname,
        status: status.status,
        message: status.message,
        tests: tests.map(test => ({
            name: test.name,
            status: statuses[test.status],
            message: test.message,
        })),
    }, location.origin);
});` });
routes.set('/', { type: html, body:
    '<!doctype html><meta charset="utf-8"><title>Sticky WPT</title><pre id="wpt-receipt"></pre><script>' +
    'const paths = ' + JSON.stringify(paths) + ';' + String.raw`
const backing = new Map();
const results = {};
addEventListener('message', event => {
    const data = event.data;
    if (event.origin !== location.origin || !backing.has(data?.path)) return;
    if (event.source !== backing.get(data.path).contentWindow) return;
    results[data.path] = data;
    if (Object.keys(results).length !== paths.length) return;
    const receipt = document.getElementById('wpt-receipt');
    receipt.textContent = JSON.stringify(paths.map(path => results[path]));
    receipt.dataset.complete = 'yes';
});
for (const path of paths) {
    const frame = document.createElement('iframe');
    frame.style.cssText = 'position:absolute;left:-3000px;top:0;width:800px;height:600px;border:0;';
    frame.src = path;
    backing.set(path, frame);
    document.body.append(frame);
}
</script>` });

const server = createServer((req, res) => {
    const route = routes.get(req.url);
    if (!route) {
        res.writeHead(req.url === '/favicon.ico' ? 204 : 404).end();
        return;
    }
    res.writeHead(200, { 'Content-Type': route.type, 'Cache-Control': 'no-store' }).end(route.body);
});
server.listen(0, '127.0.0.1');
await once(server, 'listening');

const save = (file, value) => writeFile(resolve(output, file), JSON.stringify(value, null, 2) + '\n');
try {
    const scenario = resolve(output, 'sticky-wpt-chrome-scenario.json');
    await save('sticky-wpt-chrome-scenario.json', {
        browser: {
            mode: 'launch',
            executable_path: process.env.CHROME_EXE ?? 'C:/Program Files/Google/Chrome/Application/chrome.exe',
            headless: true,
        },
        start_url: { url: 'http://127.0.0.1:' + server.address().port + '/' },
        actions: [{
            step_id: 'sticky-wpt',
            action: 'wait_for',
            state: 'attached',
            timeout_ms: 45_000,
            locator: { kind: 'css', selector: '#wpt-receipt[data-complete="yes"]' },
        }],
        capture: { at_end: ['url', 'dom'], events: ['page-errors'] },
        command_timeout_ms: 60_000,
    });
    const { stdout } = await promisify(execFile)(process.execPath, [
        rea, 'capture-browser-scenario', scenario, '--format', 'json', '--full-output',
    ], { windowsHide: true, timeout: 90_000, maxBuffer: 10 * 1024 * 1024 });
    const response = JSON.parse(stdout);
    await save('sticky-wpt-chrome-rea.json', response);
    const capture = response.data?.normalized_result;
    assert.equal(response.ok, true);
    assert.equal(capture.browser.process_ownership, 'provider-owned');
    assert.equal(capture.browser.cleanup, 'terminated-owned-process');
    const step = capture.steps.at(-1);
    assert.equal(step.status, 'completed');
    const dom = step.artifacts.dom.value.text;
    const receipt = /<pre\b(?=[^>]*\bid="wpt-receipt")(?=[^>]*\bdata-complete="yes")[^>]*>([^<]*)<\/pre>/.exec(dom);
    assert.ok(receipt, 'Missing WPT completion receipt');
    const entities = { amp: '&', lt: '<', gt: '>', quot: '"', '#39': "'" };
    const files = JSON.parse(receipt[1].replace(/&(amp|lt|gt|quot|#39);/g, (_, key) => entities[key]));
    const subtests = files.flatMap(file => file.tests);
    const passed = subtests.filter(test => test.status === 'PASS').length;
    await save('sticky-wpt-chrome.json', {
        revision: manifest.revision,
        manifest,
        evidenceId: response.data.evidence_id,
        browser: capture.browser,
        files,
        passed,
        total: subtests.length,
    });
    assert.deepEqual(files.map(file => file.path), paths);
    for (const [index, file] of files.entries()) {
        assert.equal(file.status, 0, file.path + ': ' + file.message);
        assert.equal(file.tests.length, manifest.files[index].subtests, file.path);
    }
    console.log(JSON.stringify({ passed, total: subtests.length, evidenceId: response.data.evidence_id }));
    assert.equal(passed, subtests.length);
} finally {
    server.closeAllConnections();
    await new Promise(done => server.close(done));
}
