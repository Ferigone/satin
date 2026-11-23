// Page code is served as text. The launcher does not evaluate this code.
export const written = `
trace.push('written-external');
observations.writtenExternal = [current('nested-external'), document.getElementById('external-tail') === null, document.readyState];
document.write('<span id=external-write>written by external script</span>');
`;
export const classic = `
trace.push('classic-redirect');
observations.classic = [current('classic'), document.getElementById('after') === null, document.readyState];
`;
export const sriGood = `
trace.push('sri-good');
observations.sriGood = current('sri-good');
`;
export const sriBad = "trace.push('unexpected-sri-bad');";
export const sriCross = `
trace.push('sri-cross');
observations.sriCross = current('cross-integrity');
`;
export const badMime = "trace.push('unexpected-nosniff');";
export const corsDenied = "trace.push('unexpected-cors-denied');";
export const deferFirst = `
trace.push('defer-first');
observations.deferFirst = [document.readyState, document.getElementById('after') !== null];
`;
export const deferSecond = `
trace.push('defer-second');
observations.deferSecond = [document.readyState, document.getElementById('after') !== null];
`;
export const asyncGate = `
trace.push('async-script');
observations.asyncScript = [document.readyState, document.getElementById('after') !== null];
`;
export const stylesheet = '#before{color:rgb(7,79,36)}';
export const sharedModule = `
export let count = 1;
export function increment() { count += 1; }
trace.push('module-shared');
`;
export const relativeModule = `
export const sibling = 17;
trace.push('module-relative');
`;
export const rootModule = `
import { count, increment } from 'navigation-shared';
import { sibling } from './relative.js';
class Base {
  constructor(value) { this.value = value; }
  measure() { return this.value; }
}
class Derived extends Base {
  bonus = 3;
  constructor(value) { super(value + 1); }
  measure() { return super.measure() + this.bonus; }
}
var item = new Derived(5);
var previous = count;
increment();
observations.moduleRoot = [previous, count, sibling, item.measure(), item instanceof Base, document.readyState, document.currentScript === null];
trace.push('module-root');
`;
export const crossLeaf = `
export const crossLeaf = 39;
trace.push('cross-module-leaf');
`;
export const crossEntry = `
import { crossLeaf } from './leaf.js';
export const crossValue = crossLeaf + 3;
trace.push('cross-module-root');
`;

export const streamPrefix = `<!doctype html><meta charset="utf-8"><title>HTTP stream pending</title>
<p id="stream-prefix">This part has arrived.</p>
<script id="stream-script" src="early.js"></script>`;
export const streamTailBeforeCrab = '\n<p id="stream-tail">Zażółć ';
export const streamTailAfterCrab = ' café</p><pre id="stream-result">Pending</pre>';
export const streamScript = `
var prefix = document.getElementById('stream-prefix') !== null;
var tailBeforeScript = document.getElementById('stream-tail') !== null;
var scriptState = document.readyState;
var scriptIdentity = document.currentScript === document.getElementById('stream-script');
document.addEventListener('DOMContentLoaded', function() {
  var tail = document.getElementById('stream-tail');
  var receipt = {
    prefix: prefix,
    tailBeforeScript: tailBeforeScript,
    scriptState: scriptState,
    scriptIdentity: scriptIdentity,
    readyState: document.readyState,
    tail: tail === null ? null : tail.textContent
  };
  var output = document.getElementById('stream-result');
  output.textContent = JSON.stringify(receipt);
  fetch('./result', {method:'POST', headers:{'Content-Type':'application/json'}, body:JSON.stringify(receipt)}).then(function(response) {
    output.setAttribute('data-done', response.ok ? 'yes' : 'fail');
    document.title = response.ok ? 'HTTP streaming OK' : 'HTTP streaming FAIL';
  });
});
`;
