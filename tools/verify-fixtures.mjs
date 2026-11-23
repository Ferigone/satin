import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFile } from 'node:fs/promises';

const engine = new URL('../crates/engine/', import.meta.url);
const digest = (algorithm, bytes) => createHash(algorithm).update(bytes).digest('hex');
const json = async url => JSON.parse(await readFile(url, 'utf8'));

const html = new URL('data/html5lib/', engine);
const sums = (await readFile(new URL('SHA256SUMS', html), 'utf8')).trim().split(/\r?\n/);
assert.equal(sums.length, 74);
for (const line of sums) {
  const match = /^([0-9a-f]{64}) {2}(\S+)$/.exec(line);
  assert.ok(match, 'Invalid SHA256SUMS entry');
  const [, hash, file] = match;
  assert.equal(digest('sha256', await readFile(new URL(file, html))), hash, file);
}

const svg = new URL('src/svg/fixtures/', engine);
const svgManifest = await json(new URL('manifest.json', svg));
assert.equal(svgManifest.files.filter(({ file }) => file.endsWith('.svg')).length, 14);
assert.equal(svgManifest.files.length, 15);
for (const { file, sha256 } of svgManifest.files) {
  assert.equal(digest('sha256', await readFile(new URL(file, svg))), sha256, file);
}

const data = new URL('data/data-urls/', engine);
const dataManifest = await json(new URL('manifest.json', data));
const dataBytes = await readFile(new URL(dataManifest.file, data));
assert.equal(dataBytes.length, dataManifest.bytes);
assert.equal(JSON.parse(dataBytes).length, dataManifest.cases);
assert.equal(digest('sha256', dataBytes), dataManifest.sha256);
const gitBlob = createHash('sha1').update('blob ' + dataBytes.length + '\0').update(dataBytes).digest('hex');
assert.equal(gitBlob, dataManifest.git_blob);
console.log(JSON.stringify({ html5libFiles: sums.length, svgFiles: svgManifest.files.length, dataUrlCases: dataManifest.cases }));
