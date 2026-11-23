import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { once } from 'node:events';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { resolve } from 'node:path';
import { promisify } from 'node:util';

const [exe = 'target/release/rust-browser.exe', directory = 'target/file-input-verification', expected] =
  process.argv.slice(2);
const binary = resolve(exe);
const output = resolve(directory);
const run = promisify(execFile);
const sha = bytes => createHash('sha256').update(bytes).digest('hex');
const binarySha256 = sha(await readFile(binary));
if (expected) assert.equal(binarySha256, expected.toLowerCase());
const fixture = await readFile(new URL('./fixtures/file-inputs.html', import.meta.url));
const files = [
  { name: 'one.bin', type: '', bytes: Buffer.from([0, 23, 255, 13, 10]) },
  { name: 'notatka-ł.txt', type: 'text/plain', bytes: Buffer.from('Zażółć\nπ', 'utf8') },
];
const title = 'File selection verified';
const proof = {
  events: [
    ['input', true, true, true],
    ['change', true, true, false],
    ['cancel', true, true, false],
    ['cancel', true, true, false],
  ],
  files: files.map(file => [file.name, file.type, file.bytes.length]),
  validity: true,
  value: 'C:/fakepath/one.bin',
};
await mkdir(output, { recursive: true });
const paths = files.map(file => resolve(output, file.name));
for (let i = 0; i < files.length; i++) await writeFile(paths[i], files[i].bytes);
const responses = resolve(output, 'picker-responses.json');
await writeFile(responses, JSON.stringify([paths, paths, null]));
const report = {
  ok: false, binarySha256, fixtureSha256: sha(fixture),
  runnerSha256: sha(await readFile(new URL(import.meta.url))),
  runs: {}, requests: [], errors: [],
};

function multipart(contentType, body) {
  const boundary = contentType?.match(/^multipart\/form-data;\s*boundary=(\S+)$/i)?.[1];
  assert.ok(boundary);
  const delimiter = Buffer.from('--' + boundary);
  const prefix = Buffer.from('\r\n--' + boundary);
  const parts = [];
  let offset = 0;
  while (true) {
    assert.ok(body.subarray(offset, offset + delimiter.length).equals(delimiter));
    offset += delimiter.length;
    const separator = body.subarray(offset, offset + 2).toString('ascii');
    offset += 2;
    if (separator === '--') {
      assert.equal(body.subarray(offset).toString('ascii'), '\r\n');
      return parts;
    }
    assert.equal(separator, '\r\n');
    const headersEnd = body.indexOf('\r\n\r\n', offset);
    assert.ok(headersEnd >= offset);
    const headers = body.toString('utf8', offset, headersEnd);
    const next = body.indexOf(prefix, headersEnd + 4);
    assert.ok(next >= headersEnd + 4);
    parts.push({ headers, bytes: body.subarray(headersEnd + 4, next) });
    offset = next + 2;
  }
}

async function verifyPost(request, route, mode) {
  assert.equal(request.method, 'POST');
  const chunks = [];
  let size = 0;
  for await (const chunk of request) {
    size += chunk.length;
    assert.ok(size <= 32 * 1024);
    chunks.push(chunk);
  }
  const parts = multipart(request.headers['content-type'], Buffer.concat(chunks));
  assert.equal(parts.length, 3);
  files.forEach((file, i) => {
    assert.equal(parts[i].headers, 'Content-Disposition: form-data; name="upload"; filename="' +
      file.name + '"\r\nContent-Type: ' + (file.type || 'application/octet-stream'));
    assert.deepEqual(parts[i].bytes, file.bytes);
  });
  assert.equal(parts[2].headers, 'Content-Disposition: form-data; name="proof"');
  const text = parts[2].bytes.toString('utf8');
  const receipt = text ? JSON.parse(text) : null;
  if (route === '/submit') assert.deepEqual(receipt, proof);
  else assert.equal(text, '');
  report.requests.push({
    mode, route, files: parts.slice(0, 2).map(part => ({
      bytes: part.bytes.length, sha256: sha(part.bytes),
    })),
    receipt,
  });
  return receipt;
}

function receiptPage(receipt) {
  // verifyPost checks the receipt against our fixed, owned filenames.
  const rows = receipt.files.map(([name, type, size]) =>
    '<li><strong>' + name + '</strong><span>' + size + ' bytes · ' +
    (type || 'application/octet-stream') + '</span></li>').join('');
  return '<!doctype html><meta charset=utf-8><title>' + title + '</title>' +
    '<style>body{font:18px sans-serif;color:#16352f;background:#f6fcfa;margin:36px}' +
    'ul{list-style:none;padding:0;max-width:550px}li{padding:12px;border-bottom:1px solid #85aba0}' +
    'li span{display:block;font-size:16px;color:#365950;margin-top:4px}</style>' +
    '<main><h1>' + title + '</h1><p>Fetch and form submission preserved both attachments.</p>' +
    '<ul>' + rows + '</ul><p>The repeated selection and picker dismissal preserved the selected files.</p></main>';
}

const server = createServer(async (request, response) => {
  try {
    const url = new URL(request.url, 'http://localhost');
    const mode = url.searchParams.get('run');
    if (url.pathname === '/') {
      response.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8', 'Cache-Control': 'no-store' }).end(fixture);
    } else if (['/echo', '/submit'].includes(url.pathname)) {
      assert.ok(['headless', 'native'].includes(mode));
      const receipt = await verifyPost(request, url.pathname, mode);
      if (url.pathname === '/echo') response.writeHead(204).end();
      else response.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8' })
        .end(receiptPage(receipt));
    } else {
      response.writeHead(404).end();
    }
  } catch (error) {
    report.errors.push(String(error.stack ?? error));
    response.writeHead(400).end('Incorrect submission');
  }
});
server.listen(0, '127.0.0.1');
await once(server, 'listening');
const address = 'http://127.0.0.1:' + server.address().port + '/?run=';

try {
  const nativeActions = resolve(output, 'native-actions.json');
  await writeFile(nativeActions, JSON.stringify([
    ...Array.from({ length: 3 }, () => ({ action: 'click', id: 'choose' })),
    { action: 'title', value: title },
  ]));
  for (const [mode, flags] of [
    ['headless', ['--headless', '--click-id', 'choose', '--click-count', '3', '--run-for-ms', '2500',
      '--dump-dom', resolve(output, 'headless.html')]],
    ['native', ['--smoke-window', resolve(output, 'native.png'), '--smoke-actions', nativeActions]],
  ]) {
    const result = await run(binary, ['--url', address + mode, '--profile', resolve(output, 'qa-' + randomUUID()),
      '--file-picker-responses', responses, ...flags], { timeout: 35000, windowsHide: true });
    const summary = JSON.parse(result.stdout);
    report.runs[mode] = summary;
    assert.equal(summary.title, title, JSON.stringify(summary.javascript));
    assert.deepEqual(summary.javascript.diagnostics, []);
    assert.equal(summary.layout_limited, false);
    assert.equal(summary.paint_limited, false);
    assert.deepEqual(report.requests.filter(request => request.mode === mode).map(request => request.route),
      ['/echo', '/submit']);
    if (mode === 'native') assert.ok(summary.frames_presented > 0);
  }
  assert.equal(sha(await readFile(binary)), binarySha256);
  assert.deepEqual(report.errors, []);
  report.ok = true;
} catch (error) {
  report.errors.push(String(error.stack ?? error));
  process.exitCode = 1;
} finally {
  server.closeAllConnections();
  await new Promise(done => server.close(done));
  await writeFile(resolve(output, 'report.json'), JSON.stringify(report, null, 2) + '\n');
}
console.log(JSON.stringify({ ok: report.ok, binarySha256, postRequests: report.requests.length,
  report: resolve(output, 'report.json'), errors: report.errors }));
