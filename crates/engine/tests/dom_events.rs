use rust_browser_engine::{LoadedPage, script::ScriptRuntime};
use rust_browser_javascript::Value;

const LEGACY_EVENT_PROBE: &str = r#"
(function () {
    function errorName(run) {
        try { run(); } catch (error) { return error.name; }
        return null;
    }
    const target = new EventTarget();
    const event = document.createEvent({toString() { return "hTmLeVeNtS"; }});
    const result = {
        aliases: ["Event", "Events", "HTMLEvents", "eVeNt"].every(function (name) {
            const created = document.createEvent(name);
            return Object.getPrototypeOf(created) === Event.prototype &&
                created.type === "" && !created.isTrusted && created.target === null;
        }),
        missing: errorName(function () { document.createEvent(); }),
        unknown: errorName(function () { document.createEvent("NoSuchEvent"); }),
        nonDocument: errorName(function () { document.createEvent.call(document.body, "Event"); }),
        nonEvent: errorName(function () { Event.prototype.initEvent.call({}, "test"); }),
        uninitialized: errorName(function () { target.dispatchEvent(event); }),
        uninitializedDom: errorName(function () { document.dispatchEvent(event); })
    };
    event.initEvent("first", true, true);
    target.addEventListener("first", function (dispatched) {
        let coerced = false;
        dispatched.initEvent({toString() { coerced = true; return "changed"; }}, false, false);
        result.active = coerced && dispatched.type === "first" && dispatched.bubbles &&
            dispatched.cancelable && !dispatched.isTrusted && dispatched.target === target;
        result.redispatch = errorName(function () { target.dispatchEvent(dispatched); });
        dispatched.preventDefault();
    });
    result.cancelled = !target.dispatchEvent(event) && event.defaultPrevented;
    event.stopImmediatePropagation();
    event.initEvent("");
    result.targetCleared = event.target === null;
    result.reset = event.type === "" && event.currentTarget === null &&
        !event.defaultPrevented && !event.cancelBubble && !event.bubbles && !event.cancelable &&
        !event.isTrusted;
    result.dispatchAfterReset = errorName(function () {
        if (!target.dispatchEvent(event)) throw new Error("Unexpected cancellation");
    });
    const custom = new CustomEvent("before", {composed: true, detail: 7});
    custom.initEvent("after");
    result.subclass = custom instanceof CustomEvent && custom.composed &&
        custom.detail === 7 && custom.type === "after";
    const legacyCustom = document.createEvent("cUsToMeVeNt");
    const detail = {source: "legacy"};
    result.customBrand = legacyCustom instanceof CustomEvent && legacyCustom.detail === null;
    result.customUninitialized = errorName(function () { document.dispatchEvent(legacyCustom); });
    result.nonCustomEvent = errorName(function () {
        CustomEvent.prototype.initCustomEvent.call(event, "test");
    });
    legacyCustom.initCustomEvent("legacy-custom", true, true, detail);
    document.addEventListener("legacy-custom", function (dispatched) {
        dispatched.initCustomEvent("changed", false, false, null);
        result.customDetail = dispatched === legacyCustom && dispatched.detail === detail &&
            dispatched.type === "legacy-custom" && !dispatched.isTrusted;
    });
    document.dispatchEvent(legacyCustom);
    legacyCustom.initCustomEvent("reset");
    result.customDefault = legacyCustom.detail === null && !legacyCustom.bubbles &&
        !legacyCustom.cancelable && legacyCustom.type === "reset";
    return JSON.stringify(result);
})()
"#;

#[test]
fn legacy_event_factory_and_reinitialization_follow_dispatch_state() {
    let page = LoadedPage::from_html("https://events.test/", "<!doctype html><body></body>");
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    let expected = concat!(
        r#"{"aliases":true,"missing":"TypeError","unknown":"NotSupportedError","#,
        r#""nonDocument":"TypeError","nonEvent":"TypeError","#,
        r#""uninitialized":"InvalidStateError","uninitializedDom":"InvalidStateError","#,
        r#""active":true,"redispatch":"InvalidStateError","cancelled":true,"targetCleared":true,"#,
        r#""reset":true,"dispatchAfterReset":null,"subclass":true,"customBrand":true,"#,
        r#""customUninitialized":"InvalidStateError","nonCustomEvent":"TypeError","#,
        r#""customDetail":true,"customDefault":true}"#
    );
    assert_eq!(
        runtime.eval(LEGACY_EVENT_PROBE).unwrap(),
        Value::from(expected)
    );
    assert!(runtime.diagnostics().is_empty());
}

#[test]
fn dispatches_the_same_event_through_window_document_and_element_targets() {
    let fixture = include_str!("../../../tools/fixtures/dom-events.html");
    let page = LoadedPage::from_html("https://events.test/", fixture);
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    runtime.run_initial(&page);
    runtime
        .eval("document.getElementById('run-events').click()")
        .unwrap();
    let title = runtime.eval("document.title").unwrap();
    let report = runtime
        .eval("document.getElementById('events-report').textContent")
        .unwrap();
    assert_eq!(title, Value::from("EventTarget OK"), "{report:?}");
    assert!(
        runtime.diagnostics().is_empty(),
        "{:?}",
        runtime.diagnostics()
    );
}

#[test]
fn reports_classic_script_and_timer_exceptions_to_window() {
    let fixture = include_str!("../../../tools/fixtures/window-error-reporting.html");
    let page = LoadedPage::from_html("https://events.test/errors", fixture);
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    runtime.run_initial(&page);
    runtime.pump();
    assert_eq!(
        runtime.eval("document.title").unwrap(),
        Value::from("SyntaxError,script,TypeError"),
        "{:?}",
        runtime.diagnostics()
    );
    assert_eq!(runtime.executed_scripts(), 2);
    assert!(runtime.diagnostics().is_empty());
}

#[test]
fn window_error_handler_uses_legacy_arguments_and_cancellation_only_for_error_events() {
    let page = LoadedPage::from_html("https://events.test/", "<!doctype html><body></body>");
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    assert_eq!(
        runtime
            .eval(
                r#"
                (function() {
                    var original = {};
                    var args;
                    var blank = new ErrorEvent("error");
                    var error = new ErrorEvent("error", {
                        cancelable: true, message: "example", filename: "sample.js",
                        lineno: 1, colno: -1, error: original
                    });
                    window.onerror = function() { args = Array.from(arguments); return true; };
                    if (window.dispatchEvent(error) || !error.defaultPrevented ||
                        args.length !== 5 || args[0] !== "example" || args[1] !== "sample.js" ||
                        args[2] !== 1 || args[3] !== 4294967295 || args[4] !== original) return false;
                    error.initEvent("error", false, true);
                    window.onerror = function() { return false; };
                    if (!window.dispatchEvent(error) || error.defaultPrevented) return false;
                    window.onerror = function(event) { args = [event, arguments.length]; return false; };
                    var plain = new Event("error", {cancelable: true});
                    if (window.dispatchEvent(plain) || args[0] !== plain || args[1] !== 1) return false;
                    document.body.onerror = window.onerror;
                    if (document.body.dispatchEvent(error) || args[0] !== error || args[1] !== 1) return false;
                    return blank instanceof Event && !blank.isTrusted && blank.message === "" &&
                        blank.filename === "" && blank.lineno === 0 && blank.colno === 0 &&
                        blank.error === null && !blank.cancelable;
                })()
                "#,
            )
            .unwrap(),
        Value::Bool(true),
        "{:?}",
        runtime.diagnostics()
    );
}

#[test]
fn uncanceled_errors_are_diagnosed_but_cross_origin_script_details_are_muted() {
    let mut page = LoadedPage::from_html(
        "https://events.test/errors",
        r#"<!doctype html>
        <script>
        var reports = [];
        window.onerror = function(message, filename, line, column, error) {
            reports.push([message, filename, line, column, error].join("/"));
            return false;
        };
        </script>
        <script src="https://other.test/external.js"></script>
        <script>throw "uncanceled";</script>"#,
    );
    let external = &mut std::sync::Arc::make_mut(&mut page.scripts)[1];
    external.name = "https://other.test/external.js".into();
    external.source = Some(r#"throw new Error("cross origin detail");"#.into());
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    runtime.run_initial(&page);
    assert_eq!(
        runtime.eval("reports.join('|')").unwrap(),
        Value::from(
            "Script error.//0/0/|Uncaught uncanceled/https://events.test/errors/0/0/uncanceled"
        )
    );
    assert_eq!(runtime.diagnostics().len(), 2);
    assert!(
        runtime
            .diagnostics()
            .iter()
            .any(|message| message.contains("Thrown: uncanceled"))
    );
}

#[test]
fn mouse_event_bootstrap_factory_and_native_click_share_an_interface() {
    let page = LoadedPage::from_html("https://events.test/", "<button id='target'>Go</button>");
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    let probe = r#"(() => {
        if (typeof window.MouseEvent !== "function") return false;
        const b = document.getElementById("target");
        const m = new MouseEvent("click", {bubbles:true, detail:2, view:window, clientX:-3,
            screenY: 50, button:65537, buttons:4, ctrlKey:true, relatedTarget:b});
        if (!(m instanceof UIEvent && m instanceof Event && m.detail === 2 && m.view === window &&
              m.x === -3 && m.screenY === 50 && m.button === 1 && m.buttons === 4 &&
              m.getModifierState("Control") && m.relatedTarget === b && !m.isTrusted)) return false;
        const l = document.createEvent("MouseEvents");
        let uninitialized = false;
        try { b.dispatchEvent(l); } catch(e) { uninitialized = e.name === "InvalidStateError"; }
        l.initMouseEvent("hello",true,true,window,1,2,3,4,5,false,true,false,false,1,b);
        b.addEventListener("hello", function(e) { e.initMouseEvent("changed"); e.preventDefault(); });
        if (!uninitialized || b.dispatchEvent(l) || l.type !== "hello" || l.clientY !== 5 ||
            !l.altKey || l.relatedTarget !== b || !(l instanceof MouseEvent)) return false;
        const ui = document.createEvent("UIEvents");
        ui.initUIEvent("resize",false,false,window,7);
        if (!(ui instanceof UIEvent) || ui.detail !== 7 || ui.view !== window) return false;
        window.nativeMouse = false;
        b.addEventListener("click", e => { window.nativeMouse = e instanceof MouseEvent && e.view === window; });
        return true;
    })()"#;
    assert_eq!(runtime.eval(probe).unwrap(), Value::from(true));
    runtime.click(page.document.elements("button").next().unwrap());
    assert_eq!(
        runtime.eval("window.nativeMouse").unwrap(),
        Value::from(true)
    );
}

#[test]
fn page_global_implements_window_and_dispatches_through_its_event_target() {
    let page = LoadedPage::from_html("https://window.test/", "<main></main>");
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    for check in [
        "this === window && globalThis === window && self === window && document.defaultView === window && frames === window",
        "Object.getPrototypeOf(window) === Window.prototype && window.constructor === Window && window instanceof Window && window instanceof EventTarget",
        "Object.getPrototypeOf(Window) === EventTarget && EventTarget.prototype.isPrototypeOf(Window.prototype) && !(new EventTarget() instanceof Window)",
        "Object.prototype.toString.call(window) === '[object Window]' && Object.prototype.toString.call(Window.prototype) === '[object Window]'",
        "Object.getOwnPropertyDescriptor(window,'Window').enumerable === false && typeof Object.getOwnPropertyDescriptor(window,'window').get === 'function'",
        "!Reflect.set(window, 'window', {}) && !Reflect.deleteProperty(window,'window') && window === globalThis",
        "Reflect.setPrototypeOf(window, Window.prototype) && !Reflect.setPrototypeOf(window, null) && !Reflect.setPrototypeOf(Window.prototype,null) && !Reflect.setPrototypeOf(EventTarget.prototype,null)",
        "(() => {try { new Window(); } catch (e) { return e instanceof TypeError; }})() && (() => {try { Window(); } catch (e) { return e instanceof TypeError; }})()",
        "(() => { let seen=0; const opts={once:true}; Window.prototype.addEventListener.call(window,'sample',function(e) { if(this===window && e.target===window && e.currentTarget===window) seen++; },opts); dispatchEvent(new Event('sample')); window.dispatchEvent(new Event('sample')); return seen===1; })()",
    ] {
        assert_eq!(runtime.eval(check).unwrap(), Value::Bool(true), "{check}");
    }
}
