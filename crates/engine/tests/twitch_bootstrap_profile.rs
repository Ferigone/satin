use std::{collections::HashMap, env, fs, sync::Arc, time::Instant};
use rust_browser_engine::{script::ScriptRuntime, LoadedPage};

#[test]
fn public_classic_script_replay() {
    let html = fs::read_to_string("/tmp/satin-qa-html").unwrap();
    let manifest = fs::read_to_string("/tmp/satin-qa-assets/manifest").unwrap();
    let files = manifest.lines().filter_map(|line| line.split_once('\t')).collect::<HashMap<_,_>>();
    let mut page = LoadedPage::from_html("https://www.twitch.tv/ewroon", &html);
    let mut fetched = 0;
    let mut hook = false;
    for script in Arc::make_mut(&mut page.scripts) {
        if page.document.attr(script.node,"nomodule").is_some() { script.source=None; continue; }
        if let Some(src)=page.document.attr(script.node,"src") {
            if let Some(path) = files.get(src) {
                script.source = Some(fs::read_to_string(path).unwrap());
                script.unsupported = None;
                fetched+=1;
            }
        } else if env::var_os("QA_MODULES").is_some() {
            if let Some(s)=script.source.as_mut() {
                let from="var s=i[e]={exports:{}};return f[e].call(s.exports,s,s.exports,p),s.exports";
                if s.contains(from) {
                    *s=s.replace(from,"var s=i[e]={exports:{}};__satinProfile(1,e);f[e].call(s.exports,s,s.exports,p);__satinProfile(0,e);return s.exports");
                    hook=true;
                }
            }
        }
    }
    assert!(fetched > 30, "no full set of public classic assets");
    let mut rt=ScriptRuntime::new(&page).unwrap();
    let t=Instant::now();
    rt.run_initial(&page);
    println!("SUMMARY wall={:?} fetched={fetched} executed={} modules={hook} errors={} videos={:?}", t.elapsed(),rt.executed_scripts(),rt.diagnostics().len(),rt.eval("document.getElementsByTagName('video').length"));
}
