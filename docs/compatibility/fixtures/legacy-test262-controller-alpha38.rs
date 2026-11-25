use rust_browser_engine::{Loader, script::ScriptRuntime};
use std::{
    env, fs,
    io::Write,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};
fn main() {
    let args: Vec<String> = env::args().collect();
    let cancel = AtomicBool::new(false);
    let loader = Loader::new().unwrap();
    let mut page = loader.load(&args[1], &cancel).unwrap();
    let mut rt = ScriptRuntime::for_page(&mut page).unwrap().unwrap();
    rt.eval("var observed=0, failures=[], lastPath='', completed=false; controller.implementerHook.addTestResult=function(t){observed++; if(t.result!=='pass')failures.push([observed,t.path,t.result,''+t.error]); lastPath=t.path}; controller.implementerHook.finished=function(){completed=true}").unwrap();
    let timer = Instant::now();
    let mut started = false;
    let mut previous = String::new();
    let mut changed = Instant::now();
    let mut sample = Instant::now();
    loop {
        for (id, request) in rt.take_network_requests() {
            let url = request.url.clone();
            let response = loader.web_fetch(&request, &cancel);
            eprintln!(
                "fetch {url} {:?}",
                response
                    .as_ref()
                    .map(|r| (r.status, r.body.as_ref().map(|b| b.len())))
            );
            if let Err(e) = rt.complete_network(id, response) {
                eprintln!("network completion {e:?}");
            }
        }
        rt.pump();
        if !started && rt.pending_network() == 0 && !rt.has_queued_script_tasks() {
            started = true;
            rt.eval("$('#run').click(); $('#btnRunAll').click()")
                .unwrap();
        }
        let due = rt.next_deadline();
        let now = Instant::now();
        if now >= sample {
            let text=rt.eval("[observed, failures.length, completed, lastPath, document.getElementById('totalCounter').textContent, document.getElementById('Pass').textContent, document.getElementById('Fail').textContent, document.getElementById('failedToLoadCounter').textContent, document.getElementById('nextActivity').textContent].join('|')").unwrap().as_string().unwrap().to_string_lossy();
            let finished = text.split('|').nth(2) == Some("true");
            if text != previous {
                changed = now;
                previous = text.clone();
                eprintln!("{}s {text}", timer.elapsed().as_secs());
            }
            if finished || now.duration_since(changed) > Duration::from_secs(25) {
                eprintln!(
                    "final {text}, next task {due:?}, diagnostics {:?}",
                    rt.diagnostics()
                );
                let mut output = fs::File::create(&args[2]).unwrap();
                let count = rt.eval("failures.length").unwrap().as_number().unwrap() as usize;
                for i in 0..count {
                    let row = rt.eval(&format!("JSON.stringify(failures[{i}])")).unwrap();
                    writeln!(output, "{}", row.as_string().unwrap().to_string_lossy()).unwrap();
                }
                if finished {
                    let click=rt.eval("$('#results').click(); [$('.totalCases').text(),$('.passedCases').text(),$('.failedCases').text(),$('#failedToLoadCounterDetails').text()].join('|')");
                    eprintln!("Results tab {click:?}");
                    return;
                }
                std::process::exit(2);
            }
            sample = Instant::now() + Duration::from_secs(5);
        }
        let delay = due
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
            .unwrap_or(Duration::from_millis(2))
            .min(Duration::from_millis(10));
        if !delay.is_zero() {
            std::thread::sleep(delay);
        }
    }
}
