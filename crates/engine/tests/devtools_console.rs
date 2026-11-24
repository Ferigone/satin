use rust_browser_engine::{
    LoadedPage,
    script::{
        CONSOLE_RECORD_LIMIT, CONSOLE_TEXT_BYTES, ConsoleLevel, ConsoleOrigin, ConsolePhase,
        ScriptRuntime,
    },
};
use rust_browser_javascript::{ErrorKind, Value};

use ConsoleLevel::{Debug, Error, Info, Log, Warn};
use ConsoleOrigin::{BrowserReported, ConsoleApi};

#[test]
fn console_methods_keep_severity_order_and_legacy_text_without_coercion() {
    let page = LoadedPage::from_html("https://console.test/", "<!doctype html>");
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    assert_eq!(
        runtime
            .eval(
                r#"
                var calls = 0;
                var value = {
                    toString: function () { calls++; },
                    get message() { calls++; }
                };
                value[Symbol.toPrimitive] = function () { calls++; };
                console.log("log", value, Symbol("hidden"), 42n);
                console.info("info");
                console.warn("warn");
                try { throw "caught only"; } catch (_) { console.error("page error"); }
                console.debug("debug");
                calls;
                "#,
            )
            .unwrap(),
        Value::from(0)
    );
    let snapshot = runtime.console_snapshot();
    assert_eq!(snapshot.dropped, 0);
    assert!(snapshot.records.iter().all(|record| {
        record.origin == ConsoleApi
            && !record.truncated
            && record.source.is_none()
            && record.error_kind.is_none()
            && record.line.is_none()
            && record.column.is_none()
    }));
    assert_eq!(
        snapshot
            .records
            .iter()
            .map(|record| (record.level, record.text.as_str()))
            .collect::<Vec<_>>(),
        [
            (Log, "log [object] Symbol 42n"),
            (Info, "info"),
            (Warn, "warn"),
            (Error, "page error"),
            (Debug, "debug")
        ]
    );
    assert_eq!(
        runtime.diagnostics(),
        [
            "log [object] Symbol 42n",
            "info",
            "warn",
            "page error",
            "debug"
        ]
    );
    assert_eq!(runtime.last_diagnostic().as_deref(), Some("debug"));
}

#[test]
fn only_failures_that_reach_the_reporter_are_recorded() {
    let page = LoadedPage::from_html(
        "https://console.test/",
        r#"<!doctype html>
        <button onclick="throw new TypeError('attribute')">Go</button>
        <script>
            var coercions = 0;
            var reason = {
                get message() { coercions++; },
                get name() { coercions++; },
                toString: function () { coercions++; },
                [Symbol.toPrimitive]: function () { coercions++; }
            };
            try { throw "caught throw"; } catch (_) {}
            Promise.reject("handled rejection").catch(function () {});
            Promise.reject(reason);
            queueMicrotask(function () { throw "microtask"; });
            throw reason;
        </script>"#,
    );
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    runtime.run_initial(&page);
    runtime
        .eval(
            r#"
            var button = document.querySelector("button");
            var continued = false;
            button.addEventListener("click", function () { throw "DOM listener"; });
            button.addEventListener("click", function () { continued = true; });
            var target = new EventTarget();
            var pageReporterCalls = 0;
            console.error = function () { pageReporterCalls++; };
            target.addEventListener("test", function () { throw "direct listener"; });
            target.dispatchEvent(new Event("test"));
            "#,
        )
        .unwrap();
    assert!(runtime.click(page.document.elements("button").next().unwrap()));
    assert_eq!(runtime.eval("continued").unwrap(), Value::Bool(true));
    assert_eq!(
        runtime.eval("coercions + pageReporterCalls").unwrap(),
        Value::from(0)
    );
    assert!(runtime.eval("throw 'returned only'").is_err());

    let snapshot = runtime.take_console_snapshot();
    assert_eq!(snapshot.dropped, 0);
    assert!(snapshot.records.iter().all(|record| record.level == Error));
    assert_eq!(
        snapshot
            .records
            .iter()
            .map(|record| (record.origin, record.text.as_str()))
            .collect::<Vec<_>>(),
        [
            (BrowserReported(ConsolePhase::Microtask), "microtask"),
            (
                BrowserReported(ConsolePhase::ClassicScript),
                "<thrown object>"
            ),
            (
                BrowserReported(ConsolePhase::UnhandledPromiseRejection),
                "object"
            ),
            (
                BrowserReported(ConsolePhase::EventTargetListener),
                "Thrown: direct listener"
            ),
            (BrowserReported(ConsolePhase::DomListener), "DOM listener"),
            (BrowserReported(ConsolePhase::EventHandler), "attribute"),
        ]
    );
    assert_eq!(snapshot.records[1].error_kind, Some(ErrorKind::Thrown));
    assert!(
        snapshot.records[1]
            .source
            .as_deref()
            .unwrap()
            .starts_with("script #")
    );
    assert_eq!(snapshot.records[2].error_kind, None);
    assert_eq!(snapshot.records[3].error_kind, None);
    assert_eq!(snapshot.records[4].source.as_deref(), Some("click"));
    assert_eq!(snapshot.records[5].error_kind, Some(ErrorKind::TypeError));
    assert!(
        runtime
            .diagnostics()
            .contains(&"Unhandled promise rejection: object".to_owned())
    );
    assert_eq!(runtime.diagnostics().len(), snapshot.records.len());
    runtime.pump();
    assert_eq!(runtime.console_snapshot(), Default::default());
}

#[test]
fn syntax_reports_include_known_coordinates_but_loader_diagnostics_stay_legacy() {
    let page = LoadedPage::from_html(
        "https://console.test/",
        "<!doctype html><script async src='missing.js'></script><script>var = 1;</script>\
         <button onclick='var = 1;'>Go</button>",
    );
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    runtime.run_initial(&page);
    assert!(runtime.click(page.document.elements("button").next().unwrap()));
    let snapshot = runtime.console_snapshot();
    assert_eq!(snapshot.records.len(), 2);
    let record = &snapshot.records[0];
    assert_eq!(record.origin, BrowserReported(ConsolePhase::ClassicScript));
    assert_eq!(record.error_kind, Some(ErrorKind::SyntaxError));
    assert!(record.line.is_some() && record.column.is_some());
    let record = &snapshot.records[1];
    assert_eq!(
        record.origin,
        BrowserReported(ConsolePhase::EventHandlerCompile)
    );
    assert_eq!(record.source.as_deref(), Some("click"));
    assert_eq!(record.error_kind, Some(ErrorKind::SyntaxError));
    assert_eq!(runtime.diagnostics().len(), 3);
}

#[test]
fn drain_bounds_overflow_and_unicode_without_resetting_diagnostics_or_other_realms() {
    let page = LoadedPage::from_html("https://console.test/", "<!doctype html>");
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    let other = ScriptRuntime::new(&page).unwrap();
    let args = ["'x'"; 65].join(",");
    runtime
        .eval(&format!(
            "console.log('🙂'.repeat(3000));\
             console.log({args});\
             for (var i = 2; i < {}; i++) console.log(i);\
             queueMicrotask(function () {{ throw 'overflow'; }});",
            CONSOLE_RECORD_LIMIT + 3
        ))
        .unwrap();
    runtime.pump();
    let snapshot = runtime.console_snapshot();
    assert_eq!(snapshot.records.len(), CONSOLE_RECORD_LIMIT);
    assert_eq!(snapshot.dropped, 4);
    assert_eq!(snapshot.records[0].text.len(), CONSOLE_TEXT_BYTES);
    assert!(snapshot.records[0].text.chars().all(|c| c == '🙂'));
    assert!(snapshot.records[0].truncated);
    assert!(snapshot.records[1].truncated);
    assert_eq!(snapshot.records[1].text.split(' ').count(), 64);
    assert_eq!(
        snapshot.records.last().unwrap().text,
        (CONSOLE_RECORD_LIMIT - 1).to_string()
    );
    assert_eq!(other.console_snapshot(), Default::default());
    let diagnostics = runtime.diagnostics();
    assert_eq!(diagnostics.len(), 64);
    assert_eq!(diagnostics[0].chars().count(), 1024);
    assert_eq!(runtime.take_console_snapshot(), snapshot);
    assert_eq!(runtime.console_snapshot(), Default::default());
    runtime
        .eval("console.warn('after drain'); Promise.reject('new rejection');")
        .unwrap();
    runtime.pump();
    let next = runtime.take_console_snapshot();
    assert_eq!(next.dropped, 0);
    assert_eq!(next.records.len(), 2);
    assert_eq!(next.records[0].text, "after drain");
    assert_eq!(next.records[1].text, "new rejection");
    assert_eq!(runtime.diagnostics(), diagnostics);
}

#[test]
fn developer_evaluation_uses_the_page_globals_and_previews_inert_descriptors() {
    let page = LoadedPage::from_html("https://console.test/", "<title>before</title>");
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    runtime
        .eval("var calls = 0; var obj = { answer: 42, get loud() { calls++; } };")
        .unwrap();
    let value = runtime.evaluate_console("obj").unwrap();
    assert!(
        value.contains("answer: 42") && value.contains("loud: [accessor]"),
        "{value}"
    );
    assert_eq!(runtime.evaluate_console("calls").unwrap(), "0");
    assert_eq!(
        runtime
            .evaluate_console("document.title = 'after'; 40 + 2")
            .unwrap(),
        "42"
    );
    assert_eq!(
        runtime.evaluate_console("document.title").unwrap(),
        "\"after\""
    );
    assert!(
        runtime
            .evaluate_console("unknownIdentifier")
            .unwrap_err()
            .contains("ReferenceError")
    );
    runtime
        .evaluate_console("Promise.resolve().then(() => console.info('checkpoint'))")
        .unwrap();
    assert!(
        runtime
            .take_console_snapshot()
            .records
            .iter()
            .any(|r| r.text == "checkpoint")
    );
}

#[test]
fn utility_methods_report_real_measurements_and_reset_per_document() {
    let page = LoadedPage::from_html("https://console.test/", "");
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    runtime.eval("console.assert(true); console.assert(false, 'oops'); console.count('a'); console.count('a'); console.countReset('a'); console.count('a'); console.time('t'); console.timeLog('t', 'step'); console.timeEnd('t'); console.timeEnd('missing');").unwrap();
    let batch = runtime.take_console_snapshot();
    let text: Vec<_> = batch.records.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(
        &text[..4],
        &["Assertion failed: oops", "a: 1", "a: 2", "a: 1"]
    );
    assert!(text[4].starts_with("t: ") && text[4].ends_with(" ms step"));
    assert!(text[5].starts_with("t: ") && text[5].ends_with(" ms"));
    assert_eq!(text[6], "Unknown timer: missing");
    assert_eq!(batch.records[6].level, Warn);
    let mut fresh = ScriptRuntime::new(&page).unwrap();
    fresh.eval("console.count('a')").unwrap();
    assert_eq!(fresh.take_console_snapshot().records[0].text, "a: 1");
}

#[test]
fn real_interpreted_calls_and_active_script_node_are_recorded_without_made_up_lines() {
    let mut page = LoadedPage::from_html(
        "https://console.test/",
        r#"<script>
        function inner() { console.trace("trace"); console.error("log"); throw new Error("failure"); }
        function outer() { inner(); }
        outer();
    </script>"#,
    );
    let node = page.document.elements("script").next().unwrap();
    let runtime = ScriptRuntime::for_page(&mut page).unwrap().unwrap();
    let rows = runtime.take_console_snapshot().records;
    assert_eq!(rows.len(), 3, "{rows:?}");
    for row in &rows {
        assert_eq!(row.script_node, Some(node), "{row:?}");
        let calls = row.calls.as_deref().unwrap();
        assert!(calls.len() >= 2, "{calls:?}");
        assert_eq!(&calls[..2], &["inner", "outer"]);
        assert!(row.line.is_none() && row.column.is_none());
    }
    let mut page = LoadedPage::from_html("https://console.test/", "<script>\n\nvar = ;</script>");
    let runtime = ScriptRuntime::for_page(&mut page).unwrap().unwrap();
    let row = runtime.take_console_snapshot().records.remove(0);
    assert_eq!(row.line, Some(3));
    assert!(row.calls.is_none());
    assert_eq!(
        row.script_node,
        Some(page.document.elements("script").next().unwrap())
    );
}

#[test]
fn console_selection_is_the_live_node_without_modifying_page_bindings() {
    let page = LoadedPage::from_html(
        "https://console.test/",
        "<main><button>Press</button><i></i></main>",
    );
    let button = page.document.elements("button").next().unwrap();
    let other = page.document.elements("i").next().unwrap();
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    runtime.eval("let $0 = 'page scope'; window.$0 = 'page property'; function pageValue() { return $0; }").unwrap();
    let source = "const chosen = $0; $0.setAttribute('title', 'inspected'); chosen === document.querySelector('button') && window.$0 === 'page property' && pageValue() === 'page scope'";
    assert_eq!(
        runtime
            .evaluate_console_with_selection(source, Some(button))
            .unwrap(),
        "true"
    );
    assert_eq!(
        runtime
            .evaluate_console_with_selection(
                "chosen !== $0 && $0 === document.querySelector('i')",
                Some(other)
            )
            .unwrap(),
        "true"
    );
    assert_eq!(
        runtime
            .evaluate_console_with_selection(
                "eval(String.fromCharCode(36, 48)) === chosen",
                Some(button)
            )
            .unwrap(),
        "true"
    );
    assert_eq!(
        runtime
            .evaluate_console_with_selection(
                "var callback = () => $0; callback() === chosen",
                Some(button)
            )
            .unwrap(),
        "true"
    );
    assert_eq!(
        runtime.eval("callback()").unwrap(),
        Value::from("page scope")
    );
    assert_eq!(
        runtime.eval("chosen.getAttribute('title')").unwrap(),
        Value::from("inspected")
    );
    assert_eq!(runtime.eval("$0").unwrap(), Value::from("page scope"));
    assert_eq!(
        runtime.eval("window.$0").unwrap(),
        Value::from("page property")
    );
    assert_eq!(
        runtime
            .evaluate_console_with_selection("'literal $0'", Some(button))
            .unwrap(),
        "\"literal $0\""
    );
    assert!(
        runtime
            .evaluate_console_with_selection("$0 = null", Some(button))
            .is_err()
    );
    runtime
        .eval("chosen.parentNode.removeChild(chosen)")
        .unwrap();
    assert_eq!(
        runtime
            .evaluate_console_with_selection("$0", Some(button))
            .unwrap(),
        "undefined"
    );
    assert_eq!(
        runtime
            .evaluate_console_with_selection(
                "$0",
                Some(rust_browser_engine::dom::NodeId(usize::MAX))
            )
            .unwrap(),
        "undefined"
    );
    assert_eq!(
        runtime.evaluate_console_with_selection("$0", None).unwrap(),
        "undefined"
    );
    assert_eq!(runtime.eval("$0").unwrap(), Value::from("page scope"));
}
