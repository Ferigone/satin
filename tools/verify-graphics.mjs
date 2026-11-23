// Owned HTTP fixture: tested page code executes in Rust/owned headless Chrome,
// never in Node. Input for the native window uses our internal smoke handlers.
import http from 'node:http';
import {spawn} from 'node:child_process';
import {mkdir,writeFile,readFile} from 'node:fs/promises';
import {resolve} from 'node:path';
import {createHash,randomUUID} from 'node:crypto';
import assert from 'node:assert/strict';

const binary=resolve(process.argv[2]??'target/debug/rust-browser.exe');
const output=resolve(process.argv[3]??'target/graphics-verification');
const chromeReference=process.argv.includes('--chrome-reference');
await mkdir(output,{recursive:true});
const sources=new Map(),requests=[],results=new Map();
const fixture=String.raw`<!doctype html><meta charset="utf-8"><title>Graphics pending</title>
<style>body{margin:0;font:16px Segoe UI;background:white;color:#163d32}main{margin:20px;width:600px}#box{width:100px;height:40px;padding:4px;border:2px solid #126c57;transform-origin:0 0;background:#8ae0bf}#parent{position:relative;width:200px;height:100px;padding:10px;border:2px solid black}#absolute{position:absolute;left:15px;top:10px;width:40px;height:20px;background:#287db8}#fixed{position:fixed;left:700px;top:20px;width:20px;height:20px;background:#22845f}#scroller{width:200px;height:40px;overflow:auto}#child{height:100px;background:#e5f3ec}svg{display:block}#circle{fill:#15826a;stroke:black;stroke-width:4}button{padding:8px;font-size:16px}pre{white-space:pre-wrap}</style>
<main><button type="button" id="move">Transform</button><div id="box"></div><div id="parent"><div id="absolute"></div></div>
<svg id="art" width="240" height="120" viewBox="0 0 240 120"><defs><clipPath id="clip"><rect x="0" y="0" width="100" height="120"/></clipPath><linearGradient id="gradient"><stop offset="0" stop-color="#00a070"/><stop offset="1" stop-color="#287db8"/></linearGradient></defs><path d="M 0 80 Q 40 5 80 80 T 140 80 L 140 120 L 0 120 Z" fill="url(#gradient)" clip-path="url(#clip)"/><circle id="circle" cx="180" cy="60" r="30"/></svg>
<img id="external" src="/asset.svg" width="120" height="120"><div id="scroller"><div id="child"></div></div>
<div id="fixed"></div><pre id="result">Pending</pre></main>
<script>
const RUN=__RUN__;
function check(value,message){if(!value)throw new Error(message);}
let moved=false;
const box=document.getElementById('box');
const live=getComputedStyle(box);
function change(){box.style.transform='translate(60px, 0px) rotate(90deg)';moved=true;document.title='Graphics moved';}
document.getElementById('move').addEventListener('click',change);
document.getElementById('circle').addEventListener('click',function(event){
  try{
    if(!moved)change();check(event.target===document.getElementById('circle'),'actual SVG target');
    const {width,height}=box.getBoundingClientRect();check(Math.abs(width-52)<0.01&&Math.abs(height-112)<0.01,'affine rect');
    check(box.offsetWidth===112&&box.clientWidth===108&&live.width==='100px','untransformed metrics/live styles');
    const circle=document.getElementById('circle').getBoundingClientRect();check(Math.abs(circle.width-60)<0.05&&Math.abs(circle.height-60)<0.05,'SVG object box ignores stroke');
    const scroller=document.getElementById('scroller');scroller.scrollTop=30;
    check(scroller.scrollTop===30&&scroller.scrollHeight===100,'nested scroll');
    const relative=document.getElementById('child').getBoundingClientRect().top-scroller.getBoundingClientRect().top;check(Math.abs(relative+30)<0.01,'scrolled geometry');
    const absolute=document.getElementById('absolute');check(absolute.offsetLeft===15&&absolute.offsetTop===10,'padding containing block');
    const object={value:7,read(){return this.value;}};check(object?.read?.()===7&&null?.missing===undefined,'optional receiver');
    const {value,...rest}={value:2,other:3};const [first,,...tail]=[1,2,3,4];check(value===2&&rest.other===3&&first===1&&tail.join(',')==='3,4','patterns/rest');
    let reads=0;const key=()=>{reads++;return 'value';};const target={};target[key()]??=4;check(reads===1&&target.value===4,'logical assignment reference');
    const text=String.raw` + '`line\\n${value}`' + String.raw`;check(text==='line\\n2','raw/cooked template');
    const payload={run:RUN,rect:[Math.round(width),Math.round(height)],offset:[box.offsetWidth,box.clientWidth],circle:[Math.round(circle.width),Math.round(circle.height)],absolute:[absolute.offsetLeft,absolute.offsetTop],scroll:[scroller.scrollTop,scroller.scrollHeight,Math.round(relative)],cssWidth:live.width,syntax:true,svgTarget:true};
    document.getElementById('result').textContent=JSON.stringify(payload);
    fetch('/report?run='+RUN,{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(payload)}).then(function(response){check(response.ok,'report response');document.title='Graphics pipeline OK';document.getElementById('result').setAttribute('data-ready','yes');});
  }catch(error){document.title='Graphics pipeline FAIL';document.getElementById('result').textContent=String(error);}
});
document.title='Graphics ready';
</script>`;
const asset='<svg xmlns="http://www.w3.org/2000/svg" width="120" height="120" viewBox="0 0 120 120"><style>circle{fill:#15826a}</style><circle cx="60" cy="60" r="50"/><script>fetch("/forbidden-script")</script><image href="/forbidden-image"/></svg>';
const server=http.createServer(async(req,res)=>{
  const url=new URL(req.url,'http://localhost');requests.push({method:req.method,path:req.url});
  if(url.pathname==='/asset.svg'){res.setHeader('Content-Type','image/svg+xml');res.end(asset);return;}
  if(url.pathname==='/report'){
    const chunks=[];let size=0;for await(const chunk of req){size+=chunk.length;if(size>65536){req.destroy();return;}chunks.push(chunk);}
    try{results.set(url.searchParams.get('run'),JSON.parse(Buffer.concat(chunks).toString()));res.end('OK');}
    catch{res.statusCode=400;res.end('Invalid JSON');}return;
  }
  if(url.pathname!=='/'){res.statusCode=404;res.end('Not found');return;}
  const run=url.searchParams.get('run')??'unknown';const source=fixture.replace('__RUN__',JSON.stringify(run));
  sources.set(`fixture-${run}.html`,source);res.setHeader('Content-Type','text/html;charset=utf-8');res.end(source);
});
await new Promise((done,reject)=>{server.once('error',reject);server.listen(0,'127.0.0.1',done);});
const origin=`http://127.0.0.1:${server.address().port}`;
async function run(executable,args,stem,timeout=45000){
  await writeFile(resolve(output,`${stem}-invocation.json`),JSON.stringify({executable,args,timeout},null,2));
  return new Promise((done,reject)=>{
    const child=spawn(executable,args,{windowsHide:true,stdio:['ignore','pipe','pipe']});let stdout='',stderr='',expired=false;
    const timer=setTimeout(()=>{expired=true;child.kill();},timeout);
    child.stdout.on('data',chunk=>{stdout+=chunk;if(stdout.length>32*1024*1024)child.kill();});
    child.stderr.on('data',chunk=>{stderr+=chunk;if(stderr.length>4*1024*1024)child.kill();});
    child.once('error',error=>{clearTimeout(timer);reject(error);});
    child.once('close',async(code)=>{clearTimeout(timer);try{
      await writeFile(resolve(output,`${stem}-stdout.json`),stdout);await writeFile(resolve(output,`${stem}-stderr.txt`),stderr);
      assert.equal(expired,false,`${stem} deadline`);assert.equal(code,0,`${stem}: ${stderr}`);done(JSON.parse(stdout));
    }catch(error){reject(error);}});
  });
}
const report={binary,origin,startedAt:new Date().toISOString(),requests,results:null,chrome:null,ok:false};
try{
  report.binarySha256=createHash('sha256').update(await readFile(binary)).digest('hex');
  report.headless=await run(binary,['--headless','--url',`${origin}/?run=headless`,'--click-id','circle','--run-for-ms','500','--dump-dom',resolve(output,'rust-final.html'),'--screenshot',resolve(output,'rust-headless.png')],'rust-headless');
  assert.equal(report.headless.title,'Graphics pipeline OK');assert.deepEqual(report.headless.javascript.diagnostics,[]);assert.equal(report.headless.layout_limited,false);assert.equal(report.headless.paint_limited,false);assert.ok(report.headless.svg.scenes>=2);
  const actions=[{action:'title',value:'Graphics ready'},{action:'click',id:'move'},{action:'title',value:'Graphics moved'},{action:'click',id:'circle'},{action:'title',value:'Graphics pipeline OK'}];
  const actionPath=resolve(output,'native-actions.json');await writeFile(actionPath,JSON.stringify(actions,null,2));
  report.native=await run(binary,['--url',`${origin}/?run=native`,'--smoke-window',resolve(output,'rust-native.png'),'--smoke-actions',actionPath,'--profile',resolve(output,`qa-${randomUUID()}`)],'rust-native');
  assert.equal(report.native.title,'Graphics pipeline OK');assert.ok(report.native.frames_presented>=1);assert.equal(report.native.layout_limited,false);assert.equal(report.native.paint_limited,false);assert.deepEqual(report.native.javascript.diagnostics,[]);
  assert.deepEqual(results.get('native'),{...results.get('headless'),run:'native'});
  assert.equal(requests.some(r=>r.path.startsWith('/forbidden-')),false,'SVG image must not run script or load external resources');
  if(chromeReference){
    const selector=value=>({kind:'css',selector:value});const step=(step_id,action,rest)=>({step_id,action,timeout_ms:10000,...rest});
    const scenario={browser:{mode:'launch',executable_path:'C:/Program Files/Google/Chrome/Application/chrome.exe',headless:true},start_url:{url:`${origin}/?run=chrome`},
      actions:[step('ready','wait_for',{locator:selector('#move'),state:'visible'}),step('transform','click',{locator:selector('#move')}),step('svg-click','click',{locator:selector('#circle')}),step('verified','wait_for',{locator:selector('#result[data-ready="yes"]'),state:'attached'})],
      capture:{at_end:['url','dom'],events:['page-errors','network'],network:{request_body:true,response_body:true,header_values:true}},command_timeout_ms:45000};
    const scenarioPath=resolve(output,'chrome-scenario.json');await writeFile(scenarioPath,JSON.stringify(scenario,null,2));
    const rea='C:/Users/admin/AppData/Local/npm-cache/_npx/da7caeae9db9c589/node_modules/rea-agents/scripts/rea.mjs';
    const result=await run(process.execPath,[rea,'capture-browser-scenario',scenarioPath,'--format','json','--full-output'],'chrome-rea',90000);
    const capture=result.data?.normalized_result;assert.equal(result.ok,true);assert.ok(capture.steps.every(s=>s.status==='completed'));
    assert.equal(capture.browser.cleanup,'terminated-owned-process');assert.equal(capture.browser.process_ownership,'provider-owned');
    const dom=capture.steps.at(-1).artifacts.dom;assert.equal(dom.state,'captured');assert.ok(dom.value.text.includes('<title>Graphics pipeline OK</title>'));
    await writeFile(resolve(output,'chrome-final.html'),dom.value.text);
    report.chrome={evidenceId:result.data.evidence_id,browser:capture.browser,completeness:capture.completeness,limitations:capture.limitations,steps:capture.steps.map(s=>({id:s.step_id,status:s.status}))};
    assert.deepEqual(results.get('chrome'),{...results.get('native'),run:'chrome'});
  }
  report.ok=true;
}catch(error){report.error=String(error.stack??error);process.exitCode=1;}
finally{
  report.results=Object.fromEntries(results);
  for(const[name,source]of sources)await writeFile(resolve(output,name),source);
  await writeFile(resolve(output,'asset.svg'),asset);await writeFile(resolve(output,'report.json'),JSON.stringify(report,null,2));
  server.closeAllConnections();await new Promise(done=>server.close(done));
}
process.stdout.write(JSON.stringify({ok:report.ok,title:report.native?.title,evidence:report.chrome?.evidenceId,report:resolve(output,'report.json'),error:report.error})+'\n');
