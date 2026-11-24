use super::*;

#[test]
fn custom_element_reactions_run_at_the_parser_token_and_before_dom_methods_return() {
    let (_, _, mut runtime) = start(
        r#"<!doctype html><script>
        var trace = [];
        class SiteMenu extends HTMLElement {
            connectedCallback() { trace.push('+' + this.id + (this.firstChild ? ':full' : ':empty')); }
            disconnectedCallback() { trace.push('-' + this.id); }
        }
        customElements.define('site-menu', SiteMenu);
        </script><site-menu id="parsed"><b></b></site-menu><script>
        var menu = document.getElementById('parsed');
        trace.push(menu instanceof SiteMenu ? 'parsed' : 'unupgraded');
        document.body.appendChild(menu);
        menu.remove();
        document.write('<site-menu id="written"></site-menu>');
        trace.push('returned');
        var section = document.createElement('section');
        var child = document.createElementNS('http://www.w3.org/1999/xhtml', 'site-menu');
        child.id = 'dynamic';
        section.appendChild(child);
        trace.push(child instanceof SiteMenu ? 'created' : 'unupgraded');
        document.body.appendChild(section);
        section.textContent = '';
        </script>"#,
    );
    assert_eq!(
        evaluate(&mut runtime, "trace.join('|')"),
        "+parsed:empty|parsed|-parsed|+parsed:full|-parsed|+written:empty|returned|created|+dynamic:empty|-dynamic".into()
    );
}

#[test]
fn lifecycle_listener_checkpoints_run_before_the_next_listener_and_delay_window_load() {
    let (mut page, mut session, mut runtime) = start(
        r#"<!doctype html><script>
var trace = [];
var old = document.createElement('div');
old.addEventListener('ping', function () {
    trace.push('sync-first');
    queueMicrotask(function () { trace.push('sync-job'); });
});
old.addEventListener('ping', function () { trace.push('sync-second'); });
old.dispatchEvent(new Event('ping'));
trace.push('sync-return');

var syntheticTarget;
window.addEventListener('load', function (event) { syntheticTarget = event.target; }, {once: true});
window.dispatchEvent(new Event('load'));
trace.push(syntheticTarget === window ? 'synthetic' : 'wrong-synthetic-target');

document.addEventListener('readystatechange', function () { trace.push(document.readyState); });
document.addEventListener('DOMContentLoaded', function () {
    trace.push('first-DCL');
    queueMicrotask(function () {
        trace.push('DCL-job');
        var script = document.createElement('script');
        script.src = 'boot.js';
        script.onload = function (event) {
            trace.push(event.target === script ? 'script-load' : 'wrong-script-target');
            queueMicrotask(function () { trace.push('script-job'); });
        };
        document.head.appendChild(script);
    });
});
document.addEventListener('DOMContentLoaded', function () { trace.push('second-DCL'); });
document.addEventListener('load', function () { trace.push('unexpected-document-load'); });
window.addEventListener('load', function (event) {
    var path = event.composedPath();
    trace.push(event.target === document && event.currentTarget === window &&
        this === window && path.length === 1 && path[0] === window ? 'window-load' : 'wrong-window-load');
    queueMicrotask(function () { trace.push('window-load-job'); });
});
window.addEventListener('load', function () { trace.push('second-load'); });
</script>"#,
    );
    assert!(!session.loaded());
    let before_load = "sync-first|sync-second|sync-return|synthetic|sync-job|interactive|first-DCL|DCL-job|second-DCL";
    assert_eq!(
        evaluate(&mut runtime, "trace.join('|')"),
        before_load.into()
    );

    let request = session
        .next_resource()
        .expect("dynamic script request from DCL");
    assert_eq!(request.request.url.path(), "/path/boot.js");
    complete(
        &mut page,
        &mut session,
        &mut runtime,
        request.id,
        Some((
            "boot.js",
            "trace.push(document.readyState === 'interactive' ? 'boot' : 'wrong-boot-state');",
        )),
    );
    assert!(session.done());
    assert!(session.next_resource().is_none());
    assert_eq!(
        evaluate(&mut runtime, "trace.join('|')"),
        format!("{before_load}|boot|script-load|script-job|complete|window-load|window-load-job|second-load").into()
    );
    assert!(runtime.unwrap().diagnostics().is_empty());
}
