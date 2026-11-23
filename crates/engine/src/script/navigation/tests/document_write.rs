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
            document.addEventListener("DOMContentLoaded", function () {
                log.push(document.readyState);
            });
            window.addEventListener("load", function () {
                log.push(document.readyState);
            });
            document.close();
            sameDocument && old.parentNode === null && old !== document.documentElement &&
                beforeEndTag === 0 && afterEndTag === 1 &&
                document.getElementById("tail").textContent === "tail" &&
                document.currentScript === null && document.readyState === "interactive" &&
                log.join("|") === "loading|SCRIPT";
            "#,
        )
        .unwrap();
    assert_eq!(result, Value::Bool(true));
    advance_ready(&mut page, &mut session, &mut runtime);
    assert!(session.done());
    let realm = runtime.as_mut().unwrap();
    assert_eq!(
        realm.eval("log.join('|')").unwrap(),
        "loading|SCRIPT|interactive|complete".into()
    );
    let diagnostics = realm.diagnostics();
    assert_eq!(diagnostics.len(), 2, "{diagnostics:?}");
    assert!(diagnostics[0].contains("SyntaxError"), "{diagnostics:?}");
    assert!(diagnostics[1].contains("prerequisite"), "{diagnostics:?}");
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
