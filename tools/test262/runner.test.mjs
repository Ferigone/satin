import test from 'node:test';
import assert from 'node:assert/strict';
import { metadata, variants, classify, unsupportedReason, parseArgs, moduleFixturePath } from './runner.mjs';

test('YAML metadata supports multiline descriptions, includes and both default variants', () => {
  const meta = metadata('/*---\ndescription: |\n  colon: # text\nincludes: [compareArray.js]\nfeatures:\n  - let\n---*/\nlet x = 1;');
  assert.deepEqual(meta.includes, ['compareArray.js']);
  assert.deepEqual(meta.features, ['let']);
  assert.deepEqual(variants(meta), ['sloppy', 'strict']);
  assert.deepEqual(variants(metadata('/*---\nflags: [raw]\n---*/')), ['raw']);
  assert.deepEqual(variants(metadata('/*---\nflags: [module, raw]\n---*/')), ['module']);
  assert.deepEqual(variants(metadata('/*---\nflags: [onlyStrict]\n---*/')), ['strict']);
});

test('metadata reads CR, CRLF and LF fixtures without changing script source', () => {
  for (const newline of ['\r', '\r\n', '\n']) {
    const source = ['/*---', 'description: |', '  one', '  two',
      'includes: [nativeFunctionMatcher.js]', '---*/', 'var a = 1;'].join(newline);
    const meta = metadata(source);
    assert.equal(meta.description.trimEnd(), 'one\ntwo');
    assert.deepEqual(meta.includes, ['nativeFunctionMatcher.js']);
    assert.deepEqual(variants(meta), ['sloppy', 'strict']);
  }
});

test('a harness error cannot satisfy a negative runtime test', () => {
  const meta = { negative: { phase: 'runtime', type: 'TypeError' } };
  assert.equal(classify(meta, { outcome: 'error', phase: 'setup', error: { kind: 'TypeError' } }).status, 'harness_error');
  assert.equal(classify(meta, { outcome: 'error', phase: 'parse', error: { kind: 'TypeError' } }).status, 'fail');
  assert.equal(classify(meta, { outcome: 'error', phase: 'runtime', error: { kind: 'TypeError' } }).status, 'pass');
});

test('parse negatives require the expected class and phase; unsupported grammar never passes', () => {
  const meta = { negative: { phase: 'early', type: 'SyntaxError' } };
  assert.equal(classify(meta, { outcome: 'error', phase: 'parse', error: { kind: 'SyntaxError' } }).status, 'pass');
  assert.equal(classify(meta, { outcome: 'error', phase: 'runtime', error: { kind: 'SyntaxError' } }).status, 'fail');
  assert.equal(classify(meta, { outcome: 'error', phase: 'parse', error: { kind: 'UnsupportedFeature' } }).status, 'unsupported');
  assert.equal(classify(meta, { outcome: 'error', phase: 'runtime', error: { kind: 'ExecutionLimit' } }).status, 'resource_limit');
});

test('module goals select one strict execution and async completion failures cannot pass', () => {
  assert.equal(unsupportedReason({ flags: ['async'] }, 'sloppy'), null);
  assert.equal(unsupportedReason({ flags: ['module'] }, 'module'), null);
  assert.deepEqual(variants(metadata('/*---\nflags: [module]\n---*/')), ['module']);
  assert.equal(unsupportedReason({ flags: [] }, 'strict'), null);
  assert.equal(classify({}, {outcome:'error',phase:'async',error:{kind:'AsyncIncomplete',message:'missing'}}).status, 'async_incomplete');
  assert.equal(classify({negative:{phase:'runtime',type:'TypeError'}}, {outcome:'error',phase:'async',error:{kind:'AsyncTestFailure',message:'TypeError'}}).status, 'fail');
});

test('an encountered host gap stays non-passing, including during upstream setup', () => {
  const meta = { negative: { phase: 'runtime', type: 'TypeError' } };
  const diagnostic = { kind: 'UnsupportedFeature', unsupported_host: '$262.agent' };
  for (const phase of ['setup', 'runtime']) {
    assert.deepEqual(classify(meta, { outcome: 'error', phase, error: diagnostic }), {
      status: 'unsupported', unsupportedHost: '$262.agent', reason: '$262.agent host facility is not implemented',
    });
  }
  assert.equal(classify(meta, { outcome: 'error', phase: 'setup', error: { kind: 'UnsupportedFeature' } }).status, 'harness_error');
  assert.equal(classify(meta, { outcome: 'error', phase: 'host', error: diagnostic }).status, 'harness_error');
});

test('preflight labels only the explicit unsupported Test262 host requirements', () => {
  assert.equal(unsupportedReason({ flags: [], features: ['IsHTMLDDA'] }, 'sloppy'), null);
  assert.match(unsupportedReason({ flags: ['CanBlockIsFalse'] }, 'sloppy'), /CanBlockIsFalse/);
  assert.match(unsupportedReason({ flags: ['CanBlockIsTrue'] }, 'sloppy'), /CanBlockIsTrue/);
  assert.equal(unsupportedReason({
    flags: ['CanBlockIsNotARealFlag'],
    features: ['cross-realm', 'SharedArrayBuffer', 'Symbol'],
  }, 'sloppy'), null);
});

test('invalid metadata and CLI scopes fail visibly', () => {
  assert.throws(() => metadata('no metadata'));
  assert.throws(() => metadata('/*---\nflags: [onlyStrict, noStrict]\n---*/'));
  assert.throws(() => metadata('/*---\nflags: [raw, onlyStrict]\n---*/'));
  assert.throws(() => metadata('/*---\nnegative: false\n---*/'));
  assert.throws(() => parseArgs(['--test262', 'test262', '--worker', 'rb-js']));
  const args = ['--test262', 'test262', '--worker', 'rb-js', '--path', 'language'];
  assert.throws(() => parseArgs([...args, '--limit', 'NaN']));
  assert.throws(() => parseArgs([...args, '--instructions', '100000001']), /exceeds worker limit/);
  assert.throws(() => parseArgs([...args, '--timeout-ms', '2147483648']), /exceeds Node timer range/);
});

test('module resolution negatives require the worker resolution phase', () => {
  const meta = { flags: ['module'], negative: { phase: 'resolution', type: 'SyntaxError' } };
  for (const phase of ['parse', 'runtime']) {
    assert.equal(classify(meta, { outcome: 'error', phase, error: { kind: 'SyntaxError' } }).status, 'fail');
  }
  assert.equal(classify(meta, { outcome: 'error', phase: 'resolution', error: { kind: 'SyntaxError' } }).status, 'pass');
  assert.equal(classify(meta, { outcome: 'error', phase: 'host', error: { kind: 'InternalError' } }).status, 'harness_error');
});

test('module fixture paths use exact UTF-16 specifiers and stay inside the pinned tree', () => {
  const units = text => Array.from({ length: text.length }, (_, index) => text.charCodeAt(index));
  assert.equal(moduleFixturePath('test/language/module/main.js', units('./dep_FIXTURE.js')), 'test/language/module/dep_FIXTURE.js');
  assert.equal(moduleFixturePath('test/module.js', units('./𝌆.js')), 'test/𝌆.js');
  assert.throws(() => moduleFixturePath('test/module.js', units('./../other.js')), error => error.status === 'harness_error');
  assert.throws(() => moduleFixturePath('test/module.js', units('./m.json')), error => error.status === 'unsupported');
});
