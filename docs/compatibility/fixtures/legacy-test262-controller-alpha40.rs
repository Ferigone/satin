use rust_browser_engine::{Loader, script::ScriptRuntime};
use std::{
    env,
    fs::File,
    io::{BufWriter, Write},
    path::Path,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};
const OBSERVER: &str = r#"
var observed=0, failedCount=0, finalCase='', isDone=false, pendingRows=[], cutoff='';
controller.implementerHook.addTestResult=function(t) {
  ++observed;
  var failed=t.result!=='pass';
  if (failed) ++failedCount;
  finalCase=t.path;
  pendingRows.push([observed,t.path,t.result,failed?''+t.error:'']);
  if (observed===8276) cutoff=observed+'|'+(observed-failedCount)+'|'+failedCount+'|'+t.path;
};
controller.implementerHook.finished=function(){isDone=true;};
"#;
const STATUS: &str = "[observed,failedCount,isDone,finalCase,document.getElementById('totalCounter').textContent,document.getElementById('Pass').textContent,document.getElementById('Fail').textContent,document.getElementById('failedToLoadCounter').textContent].join('|')";
fn eval_text(rt: &mut ScriptRuntime, source: &str) -> String {
    rt.eval(source)
        .unwrap()
        .as_string()
        .unwrap()
        .to_string_lossy()
}
fn main() {
    let arguments: Vec<String> = env::args().collect();
    let output = Path::new(&arguments[2]);
    let mut rows = BufWriter::new(File::create(output.join("callback-batches.jsonl")).unwrap());
    let loader = Loader::new().unwrap();
    let cancel = AtomicBool::new(false);
    let mut page = loader.load(&arguments[1], &cancel).unwrap();
    let by_id = |id: &str| {
        page.document
            .connected_nodes()
            .find(|&n| page.document.attr(n, "id") == Some(id))
            .unwrap()
    };
    let (run, run_all, results) = (by_id("run"), by_id("btnRunAll"), by_id("results"));
    let mut rt = ScriptRuntime::for_page(&mut page).unwrap().unwrap();
    rt.eval(OBSERVER).unwrap();
    let timer = Instant::now();
    let mut next_sample = timer;
    let mut progress = timer;
    let mut previous = String::new();
    let mut started = false;
    let mut checkpoint = false;
    loop {
        for (id, req) in rt.take_network_requests() {
            let response = loader.web_fetch(&req, &cancel);
            eprintln!(
                "fetch {} {:?}",
                req.url,
                response.as_ref().map(|r| r.status)
            );
            rt.complete_network(id, response).unwrap();
        }
        rt.pump();
        if !started && rt.pending_network() == 0 && !rt.has_queued_script_tasks() {
            started = true;
            let run_default = rt.click(run);
            let run_all_default = rt.click(run_all);
            eprintln!(
                "activated Run and Run All native events, default behavior {run_default}, {run_all_default}"
            );
        }
        let due = rt.next_deadline();
        let now = Instant::now();
        if now >= next_sample {
            let status = eval_text(&mut rt, STATUS);
            let batch = eval_text(
                &mut rt,
                "(function(){var a=pendingRows;pendingRows=[];return JSON.stringify(a)})()",
            );
            if batch != "[]" {
                writeln!(rows, "{batch}").unwrap();
                rows.flush().unwrap();
            }
            let fields: Vec<_> = status.split('|').collect();
            if !checkpoint && fields[0].parse::<usize>().unwrap() >= 8276 {
                let value = eval_text(&mut rt, "cutoff");
                std::fs::write(output.join("checkpoint.txt"), format!("{value}\n")).unwrap();
                eprintln!("checkpoint {value}");
                checkpoint = true;
            }
            if status != previous {
                progress = now;
                previous = status.clone();
                eprintln!("{}s {status}", timer.elapsed().as_secs());
            }
            if fields[2] == "true" || now.duration_since(progress) > Duration::from_secs(60) {
                eprintln!(
                    "final {status}, next task {due:?}, diagnostics {:?}",
                    rt.diagnostics()
                );
                if fields[2] != "true" {
                    std::process::exit(2);
                }
                let base = rt.diagnostics().len();
                let allowed = rt.click(results);
                rt.snapshot(&mut page);
                let document = &page.document;
                let has_class = |node, name: &str| {
                    document
                        .attr(node, "class")
                        .unwrap_or("")
                        .split_ascii_whitespace()
                        .any(|v| v == name)
                };
                let class_text = |name: &str| {
                    document
                        .connected_nodes()
                        .find(|&node| has_class(node, name))
                        .map(|node| document.text(node))
                        .unwrap_or_default()
                };
                let load = document
                    .connected_nodes()
                    .find(|&node| document.attr(node, "id") == Some("failedToLoadCounterDetails"))
                    .map(|node| document.text(node))
                    .unwrap_or_default();
                let report = document
                    .connected_nodes()
                    .find(|&node| has_class(node, "results-data-table"));
                let mut pending: Vec<_> = report.into_iter().collect();
                let (mut report_rows, mut links) = (0usize, 0usize);
                while let Some(node) = pending.pop() {
                    pending.extend(document.nodes[node.0].children.iter().copied());
                    report_rows += usize::from(document.tag(node) == "tr");
                    links +=
                        usize::from(document.tag(node) == "a" && has_class(node, "showSource"));
                }
                let result = format!(
                    "{}|{}|{}|{load}|{report_rows}|{links}",
                    class_text("totalCases"),
                    class_text("passedCases"),
                    class_text("failedCases")
                );
                std::fs::write(output.join("results.html"), document.serialize()).unwrap();
                eprintln!(
                    "Results native input default {allowed}, counters+rows+links {result}, new diagnostics {:?}",
                    rt.diagnostics().get(base..)
                );
                std::fs::write(output.join("results.txt"), format!("{result}\n")).unwrap();
                return;
            }
            next_sample = Instant::now() + Duration::from_secs(5);
        }
        let delay = due
            .map(|d| d.saturating_duration_since(Instant::now()))
            .unwrap_or(Duration::from_millis(2))
            .min(Duration::from_millis(10));
        if !delay.is_zero() {
            std::thread::sleep(delay);
        }
    }
}
