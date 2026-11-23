use super::*;

#[test]
fn script_opened_document_runs_split_classic_scripts_before_close() {
    let (mut page, mut session, mut runtime) =
        start("<!doctype html><title>old document</title><script>var log = [];</script>");
    assert!(session.done());
    let realm = runtime.as_mut().unwrap();
    let result = realm
        .eval(
            r#"
            var old = document.documentElement;
            old.addEventListener("probe", function () { log.push("old element"); });
            document.addEventListener("probe", function () { log.push("old document"); });
            window.addEventListener("probe", function () { log.push("old window"); });
            window.onload = function () { log.push("old handler"); };
            var sameDocument = document.open() === document;
            old.dispatchEvent(new Event("probe"));
            document.dispatchEvent(new Event("probe"));
            window.dispatchEvent(new Event("probe"));
            document.writeln("<script type='text/javascript'>");
            document.writeln("log.push(document.readyState);");
            var beforeEndTag = log.length;
            document.writeln("</script>");
            var afterEndTag = log.length;
            document.writeln("<script type='text/javascript'>");
            document.writeln("va xyz =");
            document.writeln("</script>");
            document.writeln("<script type='text/javascript'>");
            document.writeln("throw Error('prerequisite');");
            document.writeln("</script>");
            document.writeln("<script type='text/javascript'>");
            document.writeln("log.push(document.currentScript.tagName);");
            document.writeln("</script>");
            document.writeln("<p id=tail>tail</p>");
            document.addEventListener("readystatechange", function () {
                log.push("ready:" + document.readyState);
                if (document.readyState === "interactive") {
                    queueMicrotask(function () { log.push("ready-job"); });
                }
            });
            document.addEventListener("readystatechange", function () {
                log.push("second-ready:" + document.readyState);
            });
            document.addEventListener("DOMContentLoaded", function () {
                log.push("dcl:" + document.readyState);
            });
            window.addEventListener("load", function (event) {
                log.push("load:" + document.readyState + ":" + (event.target === document));
            });
            document.close();
            log.push("returned:" + document.readyState);
            sameDocument && old.parentNode === null && old !== document.documentElement &&
                beforeEndTag === 0 && afterEndTag === 1 &&
                document.getElementById("tail").textContent === "tail" &&
                document.currentScript === null && document.readyState === "interactive" &&
                log.join("|") === "loading|SCRIPT|ready:interactive|second-ready:interactive|returned:interactive";
            "#,
        )
        .unwrap();
    assert_eq!(result, Value::Bool(true));
    // eval() was the author caller. End its task before driving the queued
    // navigation events; do not checkpoint inside close()'s ready listeners.
    realm.drain_microtasks();
    advance_ready(&mut page, &mut session, &mut runtime);
    assert!(session.done());
    let realm = runtime.as_mut().unwrap();
    assert_eq!(
        realm.eval("log.join('|')").unwrap(),
        "loading|SCRIPT|ready:interactive|second-ready:interactive|returned:interactive|ready-job|dcl:interactive|ready:complete|second-ready:complete|load:complete:true".into()
    );
    let diagnostics = realm.diagnostics();
    assert_eq!(diagnostics.len(), 2, "{diagnostics:?}");
    assert!(diagnostics[0].contains("SyntaxError"), "{diagnostics:?}");
    assert!(diagnostics[1].contains("prerequisite"), "{diagnostics:?}");
}

#[test]
fn written_deferred_script_delays_readiness_events_but_not_the_close_caller() {
    let (mut page, mut session, mut runtime) =
        start("<!doctype html><script>var log = [];</script>");
    assert_eq!(
        evaluate(
            &mut runtime,
            r#"
            document.open();
            document.write("<!doctype html><script defer src='deferred.js'></script>");
            document.addEventListener("DOMContentLoaded", function () {
                log.push("dcl:" + document.readyState);
            });
            window.addEventListener("load", function () {
                log.push("load:" + document.readyState);
            });
            document.close();
            document.close();
            document.readyState;
            "#,
        ),
        "interactive".into()
    );
    advance_ready(&mut page, &mut session, &mut runtime);
    assert!(!session.loaded());
    assert_eq!(evaluate(&mut runtime, "log.length"), 0.into());
    let request = session.next_resource().expect("written deferred script");
    assert_eq!(request.request.url.path(), "/path/deferred.js");
    complete(
        &mut page,
        &mut session,
        &mut runtime,
        request.id,
        Some(("deferred.js", "log.push('defer:' + document.readyState);")),
    );
    assert!(session.done());
    assert_eq!(
        evaluate(&mut runtime, "log.join('|')"),
        "defer:interactive|dcl:interactive|load:complete".into()
    );
    assert!(runtime.unwrap().diagnostics().is_empty());
}

#[test]
fn nested_close_does_not_end_the_active_script_insertion_point() {
    let (mut page, mut session, mut runtime) =
        start("<!doctype html><script>var log = [];</script>");
    assert_eq!(
        evaluate(
            &mut runtime,
            r#"
            document.open();
            document.write("<!doctype html>");
            document.addEventListener("DOMContentLoaded", function () { log.push("dcl"); });
            window.addEventListener("load", function () { log.push("load"); });
            document.write('<script id="nested">' +
                'document.open();' +
                'log.push(document.currentScript.id);' +
                'document.close();' +
                'log.push(document.readyState);' +
                'document.write("<p id=inside>inside</p>");' +
                '</script><p id=outside>outside</p>');
            document.close();
            document.close();
            document.body.children[0].id === "inside" &&
                document.body.children[1].id === "outside" &&
                document.currentScript === null;
            "#,
        ),
        Value::Bool(true)
    );
    advance_ready(&mut page, &mut session, &mut runtime);
    assert!(session.done());
    evaluate(&mut runtime, "document.close()");
    advance_ready(&mut page, &mut session, &mut runtime);
    assert_eq!(
        evaluate(&mut runtime, "log.join('|')"),
        "nested|loading|dcl|load".into()
    );
    assert!(runtime.unwrap().diagnostics().is_empty());
}

#[test]
fn closing_a_written_document_waits_for_its_pending_classic_script() {
    let (mut page, mut session, mut runtime) = start(
        r#"<!doctype html><script>
        var log = [];
        log.push(document.open() === document && document.currentScript !== null);
        </script><p id=old>old</p>"#,
    );
    assert!(session.done());
    evaluate(
        &mut runtime,
        r#"document.write("<!doctype html><script src='blocking.js'></script>");
        document.write("<p id=middle>middle</p>");"#,
    );
    advance_ready(&mut page, &mut session, &mut runtime);
    let request = session.next_resource().unwrap();
    assert_eq!(request.request.url.path(), "/path/blocking.js");
    assert_eq!(
        evaluate(
            &mut runtime,
            r#"document.writeln("<p id=tail>tail</p>");
            document.close();
            document.getElementById("old") === null &&
                document.getElementById("middle") === null &&
                document.getElementById("tail") === null &&
                document.readyState === "loading";"#,
        ),
        Value::Bool(true)
    );
    advance_ready(&mut page, &mut session, &mut runtime);
    assert!(session.next_resource().is_none());
    complete(
        &mut page,
        &mut session,
        &mut runtime,
        request.id,
        Some((
            "blocking.js",
            r#"log.push(document.getElementById("middle") === null);
            document.write("<p id=from-script>ahead of middle and tail</p>");"#,
        )),
    );
    assert!(session.done());
    assert_eq!(
        evaluate(
            &mut runtime,
            r#"log.join("|") === "true|true" &&
                document.body.children[0].id === "from-script" &&
                document.body.children[1].id === "middle" &&
                document.body.children[2].id === "tail" &&
                document.readyState === "complete";"#,
        ),
        Value::Bool(true)
    );
    assert!(runtime.unwrap().diagnostics().is_empty());
}
