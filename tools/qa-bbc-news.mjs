import {execFile} from 'node:child_process';
import {promisify} from 'node:util';
import {createRequire} from 'node:module';
import {mkdir, mkdtemp, writeFile, rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {resolve, join} from 'node:path';
import {performance} from 'node:perf_hooks';
const [satin, chrome, pkg] = process.argv.slice(2).map(x => resolve(x));
const run = promisify(execFile);
const {chromium} = createRequire(pkg)('playwright-core');
const evidence = resolve('target/bbc-evidence');
const tmp = await mkdtemp(join(tmpdir(),'satin-bbc-'));
await mkdir(evidence,{recursive:true});
const viewport = {width:1280,height:800};
const urls = ['https://www.bbc.com/','https://www.bbc.com/news','https://www.bbc.com/news/articles/crpdw1qx7jgxo'];
const safe = value => String(value ?? '').replace(/https?:\/\/[^\s"'<>]+/g, match => {
  try {const u=new URL(match); return u.origin+u.pathname;} catch {return '[url]';}
}).slice(0,1000);
const report = {commit:process.env.GITHUB_SHA, startedUtc:new Date().toISOString(), viewport, nativeWindow:{width:1280,height:945, chromeHeight:122, statusHeight:22, note:'Verify measured content viewport, not just window size'}, cases:[]};
const save = async () => {await writeFile(join(evidence,'summary.json'),JSON.stringify(report,null,2)); console.log(JSON.stringify(report));};
try {
  const browser = await chromium.launch({executablePath:chrome,headless:true});
  try {
    report.chromeVersion=browser.version();
    for(const [i,url] of urls.entries()){
      const item={url,chrome:{console:[],networkErrors:[]},satin:[]};
      report.cases.push(item);
      const ctx=await browser.newContext({viewport,deviceScaleFactor:1,locale:'en-US'});
      try{
        const page=await ctx.newPage();
        page.on('pageerror',e=>{if(item.chrome.console.length<30)item.chrome.console.push(safe(e.message))});
        page.on('console',m=>{if(m.type()==='error'&&item.chrome.console.length<30)item.chrome.console.push(safe(m.text()))});
        page.on('requestfailed',r=>{if(item.chrome.networkErrors.length<30)item.chrome.networkErrors.push({url:safe(r.url()),error:safe(r.failure()?.errorText)})});
        page.on('response',r=>{if(r.status()>=400&&item.chrome.networkErrors.length<30)item.chrome.networkErrors.push({url:safe(r.url()),status:r.status()})});
        const start=performance.now();
        try{item.chrome.status=(await page.goto(url,{waitUntil:'domcontentloaded',timeout:25000}))?.status()}catch(e){item.chrome.navigationError=safe(e.message)}
        item.chrome.navigationMs=performance.now()-start;
        await page.waitForTimeout(3000);
        item.chrome.title=await page.title(); item.chrome.finalUrl=safe(page.url());
        await page.screenshot({path:join(evidence,'chrome-'+i+'-initial.png'),timeout:10000});
        item.chrome.consentDialog=await page.locator('iframe[title="SP Consent Message"]:visible').count()>0;
        item.chrome.initialScroll=await page.evaluate(()=>scrollY);
        const scrollStart=performance.now();
        for(let j=0;j<8;j++)await page.keyboard.press('ArrowDown');
        await page.waitForTimeout(500);
        item.chrome.scrollElapsedMs=performance.now()-scrollStart;
        item.chrome.finalScroll=await page.evaluate(()=>scrollY);
        await page.screenshot({path:join(evidence,'chrome-'+i+'-scroll.png'),timeout:10000});
        item.chrome.articleCandidate=await page.locator('a[href*="/news/articles/"]:visible').first().getAttribute('href',{timeout:3000}).catch(()=>null);
        if(item.chrome.articleCandidate&&!item.chrome.consentDialog){try{
          await page.locator('a[href*="/news/articles/"]:visible').first().click({timeout:5000});
          await page.waitForTimeout(1500);item.chrome.articleUrl=safe(page.url());
          await page.screenshot({path:join(evidence,'chrome-'+i+'-article.png'),timeout:10000});
          await page.goBack({waitUntil:'domcontentloaded',timeout:10000});
          item.chrome.backUrl=safe(page.url());
        }catch(e){item.chrome.articleError=safe(e.message)}}else if(item.chrome.consentDialog){item.chrome.articleError='Consent dialog blocks ordinary navigation; separate direct article case is not a click pass';}
      }catch(e){item.chrome.error=safe(e.message)}finally{await ctx.close()}
      for(const phase of ['initial','scroll']){
        const actions=join(tmp,i+'-'+phase+'.json');
        await writeFile(actions,JSON.stringify(phase==='scroll'?Array.from({length:8},()=>({action:'key',value:'ArrowDown'})):[]));
        const result={phase};item.satin.push(result);
        const start=performance.now();
        try{
          const {stdout,stderr}=await run(satin,['--url',url,'--profile',join(tmp,'satin-'+i+'-'+phase),'--smoke-window',join(evidence,'satin-'+i+'-'+phase+'.png'),'--smoke-actions',actions,'--width','1280','--height','945'],{timeout:120000,maxBuffer:8*1024*1024,windowsHide:true,env:{...process.env,SATIN_QA_LAYOUT_LIMIT:'1'}});
          const j=JSON.parse(stdout.trim());result.version=j.version;result.finalUrl=safe(j.url);result.title=j.title;result.frames=j.frames_presented;result.windowSmoke=j.window_smoke;result.layoutLimited=j.layout_limited;result.paintLimited=j.paint_limited;result.scrollY=j.scroll_y;result.contentViewport=j.content_viewport;result.layoutReasons=[...new Set(stderr.split(/\r?\n/).filter(l=>l.includes('SATIN_QA_LAYOUT_LIMIT')))].slice(0,30);result.diagnostics=j.javascript?.diagnostics?.slice(0,30).map(safe);result.stderr=safe(stderr);
        }catch(e){result.error=safe(e.message);result.stderr=safe(e.stderr);result.exit=e.code}
        result.elapsedMs=performance.now()-start;
        await save();
      }
    }
  } finally {await browser.close()}
} finally {await save();await rm(tmp,{recursive:true,force:true,maxRetries:3})}
