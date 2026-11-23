// The Node process serves owned fixtures and orchestrates our Rust binary.
// Tested page JavaScript runs only in the Rust browser (optionally in owned
// headless Chrome as a reference), never in Node's evaluator.
import http from 'node:http';
import { spawn } from 'node:child_process';
import { mkdir, writeFile, readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import assert from 'node:assert/strict';

const binary = resolve(process.argv[2] ?? 'target/debug/rust-browser.exe');
const output = resolve(process.argv[3] ?? 'target/web-verification');
await mkdir(output, {recursive:true});
const requests=[];
let pageOrigin='';
const cross=http.createServer((request,response)=>{
  requests.push({server:'cross',method:request.method,path:request.url,headers:request.headers});
  if(request.url==='/denied'){response.end('private');return;}
  response.setHeader('Access-Control-Allow-Origin',pageOrigin);
  if(request.method==='OPTIONS'){
    response.setHeader('Access-Control-Allow-Methods','PUT');
    response.setHeader('Access-Control-Allow-Headers','content-type,x-custom');
    response.statusCode=204;response.end();return;
  }
  response.setHeader('Content-Type','application/json');
  response.setHeader('Access-Control-Expose-Headers','X-Visible');
  response.setHeader('X-Visible','yes');response.setHeader('X-Hidden','private');
  response.end('{"cross":true}');
});
await new Promise(resolveListen=>cross.listen(0,'127.0.0.1',resolveListen));
const crossOrigin=`http://127.0.0.1:${cross.address().port}`;
const source=`<!doctype html><meta charset="utf-8"><title>Pipeline pending</title>
<style>body{font-family:Segoe UI;margin:32px;background:#f8fafc;color:#18332d}#cards{display:grid;grid-template-columns:repeat(2,1fr);gap:16px;max-width:800px}article{padding:20px;background:#eaf6f0;border:1px solid #9acbbb}h1{font-size:36px}pre{white-space:pre-wrap}</style>
<h1 id="heading">Oczekiwanie na Fetch…</h1><div id="cards"></div><pre id="result"></pre>
<template id="card"><article><h2>Szablon HTML</h2><p>Wstawiony przez własny DOM.</p></article></template>
<script>
function check(value,message){if(!value)throw new Error(message);}
var order=[];var dataResult={};
Promise.resolve().then(function(){order.push('promise');});
queueMicrotask(function(){order.push('microtask');});
document.getElementById('cards').appendChild(document.getElementById('card').content.cloneNode(true));
document.getElementById('cards').insertAdjacentHTML('beforeend','<article id="dynamic"><h2>Fragment HTML</h2><p>Druga karta układu Grid.</p></article>');
fetch('/api/data').then(function(response){
  check(response.status===200,'status');check(response.headers.get('x-marker')==='owned-fixture','header');
  var copy=response.clone();var parsed=response.json();check(response.bodyUsed,'bodyUsed');
  return parsed.then(function(data){dataResult.message=data.message;return copy.text();});
}).then(function(text){
  check(JSON.parse(text).message==='Rust Fetch OK','clone text');
  var headers=new Headers({'Content-Type':'application/json','X-Custom':'test'});
  return fetch(${JSON.stringify(`${crossOrigin}/allowed`)},{method:'PUT',headers:headers,body:JSON.stringify({value:42})});
}).then(function(response){
  check(response.type==='cors','cors type');check(response.headers.get('x-visible')==='yes','exposed');
  check(response.headers.get('x-hidden')===null,'hidden');return response.json();
}).then(function(data){
  check(data.cross,'cross JSON');
  return fetch(${JSON.stringify(`${crossOrigin}/denied`)}).then(function(){throw new Error('CORS incorrectly exposed');},function(error){check(error instanceof TypeError,'CORS rejection class');dataResult.denied=true;});
}).then(function(){
  return fetch(${JSON.stringify(`${crossOrigin}/denied`)},{mode:'no-cors'});
}).then(function(response){
  check(response.type==='opaque'&&response.status===0&&response.headers.get('x-hidden')===null,'opaque');
  return response.text();
}).then(function(empty){
  check(empty==='','opaque body');check(order.join(',')==='promise,microtask','shared FIFO');
  check(document.querySelectorAll('#cards article').length===2,'fragment template');
  dataResult.order=order;dataResult.cards=2;dataResult.ok=true;
  document.getElementById('heading').textContent=dataResult.message;
  document.getElementById('result').textContent=JSON.stringify(dataResult);
  document.title='Web pipeline OK';
}).catch(function(error){document.title='Web pipeline FAIL';document.getElementById('result').textContent=String(error);});
</script>`;
const page=http.createServer((request,response)=>{
  requests.push({server:'page',method:request.method,path:request.url,headers:request.headers});
  if(request.url==='/api/data'){
    response.setHeader('Content-Type','application/json');response.setHeader('X-Marker','owned-fixture');
    response.end('{"message":"Rust Fetch OK"}');return;
  }
  response.setHeader('Content-Type','text/html; charset=utf-8');response.end(source);
});
await new Promise(resolveListen=>page.listen(0,'127.0.0.1',resolveListen));
pageOrigin=`http://127.0.0.1:${page.address().port}`;

function run(args){
  return new Promise((resolveRun,reject)=>{
    const child=spawn(binary,args,{windowsHide:true,stdio:['ignore','pipe','pipe']});
    let stdout='',stderr='';
    const timeout=setTimeout(()=>{child.kill();reject(new Error('Owned browser test exceeded 30 s'));},30000);
    child.stdout.on('data',chunk=>{stdout+=chunk;});child.stderr.on('data',chunk=>{stderr+=chunk;});
    child.on('error',error=>{clearTimeout(timeout);reject(error);});
    child.on('exit',code=>{clearTimeout(timeout);if(code!==0)reject(new Error(`Browser exit${code}: ${stderr}`));else{try{resolveRun(JSON.parse(stdout));}catch(error){reject(new Error(`Invalid browser report: ${stdout} ${error}`));}}});
  });
}
try {
  await writeFile(resolve(output,'fixture.html'),source);
  const headless=await run(['--headless','--url',`${pageOrigin}/app.html`,'--run-for-ms','5000','--dump-dom',resolve(output,'result.html'),'--screenshot',resolve(output,'page.png'),'--height','1200']);
  const html=await readFile(resolve(output,'result.html'),'utf8');
  await writeFile(resolve(output,'headless-report.json'),JSON.stringify(headless,null,2));
  assert.equal(headless.title,'Web pipeline OK',html);
  assert.equal(headless.javascript.pending_requests,0);
  assert.deepEqual(headless.javascript.diagnostics,[]);
  assert.match(html,/'?"ok":true/);
  const native=await run(['--smoke-window',resolve(output,'native-window.png'),'--url',`${pageOrigin}/app.html`,'--profile',resolve(output,'native profile')]);
  assert.equal(native.title,'Web pipeline OK');assert.ok(native.frames_presented>=1);
  const preflight=requests.findIndex(request=>request.server==='cross'&&request.method==='OPTIONS');
  const put=requests.findIndex(request=>request.server==='cross'&&request.method==='PUT');
  assert.ok(preflight>=0&&put>preflight);
  const report={headless,native,checks:['own Promise/queueMicrotask FIFO','JSON body+clone','template clone+fragment insertion','Grid render','CORS preflight/exposed headers','CORS denial','opaque response','headless and native async transport'],requests};
  await writeFile(resolve(output,'report.json'),JSON.stringify(report,null,2));
  process.stdout.write(`${JSON.stringify({report:resolve(output,'report.json'),title:headless.title,nativeTitle:native.title,requests:requests.length,frames:native.frames_presented})}\n`);
} finally {
  page.closeAllConnections();cross.closeAllConnections();
  await Promise.all([new Promise(resolveClose=>page.close(resolveClose)),new Promise(resolveClose=>cross.close(resolveClose))]);
}
