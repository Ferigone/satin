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
fn mouse_page_coordinates_follow_scroll_and_stay_fixed_during_dispatch() {
    let page = LoadedPage::from_html(
        "https://events.test/",
        "<!doctype html><body style='width:2400px;height:3600px'><button id='target'>Go</button>",
    );
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    let result = runtime
        .eval(
            r#"(() => {
        scrollTo(100, 200);
        const event = new MouseEvent('probe', { view: window, clientX: 7, clientY: -3 });
        const detached = new MouseEvent('probe', { clientX: 7, clientY: -3 });
        const getter = Object.getOwnPropertyDescriptor(MouseEvent.prototype, 'pageX').get;
        let brand = false;
        try { getter.call(new Event('probe')); } catch(e) { brand = e instanceof TypeError; }
        const observations = [brand, event.pageX, event.pageY, detached.pageX, detached.pageY];
        const target = new EventTarget();
        target.addEventListener('probe', e => {
            scrollTo(130, 250);
            observations.push(e.pageX, e.pageY);
        });
        target.dispatchEvent(event);
        observations.push(event.pageX, event.pageY);
        const legacy = document.createEvent('MouseEvents');
        legacy.initMouseEvent('legacy', true, true, window, 0, 0, 0, -5, 11);
        document.getElementById('target').addEventListener('legacy', e => {
            scrollTo(0, 0);
            observations.push(e.pageX, e.pageY);
        });
        document.getElementById('target').dispatchEvent(legacy);
        observations.push(legacy.pageX, legacy.pageY);
        return observations.join('|');
    })()"#,
        )
        .unwrap();
    assert_eq!(
        result,
        Value::from("true|107|197|7|-3|107|197|137|247|125|261|-5|11")
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

#[test]
fn native_click_and_form_input_allow_bounded_work_and_flush_microtasks() {
    let page = LoadedPage::from_html(
        "https://events.test/input",
        "<button id='go'>Go</button><input id='field'>",
    );
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    let button = page.document.elements("button").next().unwrap();
    let field = page.document.elements("input").next().unwrap();
    runtime
        .eval(
            r#"
        var blank = ' '.repeat(8000), completed = '';
        function work() {
            for (var i = 0; i < 150; ++i) blank.repeat(1);
            var kind = this.id;
            queueMicrotask(function() { completed += kind; });
        }
        document.getElementById('go').addEventListener('click', work);
        document.getElementById('field').addEventListener('input', work);
    "#,
        )
        .unwrap();
    assert!(runtime.click(button));
    runtime.focus_control(Some(field)).unwrap();
    assert!(runtime.insert_form_text("x").unwrap());
    assert_eq!(runtime.eval("completed").unwrap(), Value::from("gofield"));
    assert!(runtime.diagnostics().is_empty());

    runtime
        .eval(
            r#"
        var button = document.getElementById('go');
        button.removeEventListener('click', work);
        function tooMuch() {
            for (;;) blank.repeat(1);
        }
        button.addEventListener('click', tooMuch);
    "#,
        )
        .unwrap();
    assert!(!runtime.click(button));
    assert!(
        runtime
            .last_diagnostic()
            .unwrap()
            .contains("exceeded")
    );
    runtime
        .eval("button.removeEventListener('click', tooMuch); button.addEventListener('click', work); completed = ''")
        .unwrap();
    assert!(runtime.click(button));
    assert_eq!(runtime.eval("completed").unwrap(), Value::from("go"));
}

#[test]
fn reentrant_synthetic_click_completes_under_native_input_but_inherits_host_eval_limit() {
    let page = LoadedPage::from_html(
        "https://events.test/reentrant",
        "<button id='outer'></button><button id='inner'></button>",
    );
    let outer = page.document.elements("button").next().unwrap();
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    runtime
        .eval(
            r#"
        var blank = ' '.repeat(8000), marker = '';
        var inner = document.getElementById('inner'), outer = document.getElementById('outer');
        function spend() { for (var i = 0; i < 1500; i++) blank.repeat(1); }
        inner.addEventListener('click', function() {
            spend();
            marker = 'inner';
            queueMicrotask(function() { spend(); marker = 'finished'; });
        });
        outer.addEventListener('click', function() { spend(); inner.click(); });
    "#,
        )
        .unwrap();
    assert!(runtime.click(outer));
    assert_eq!(runtime.eval("marker").unwrap(), Value::from("finished"));
    assert!(runtime.diagnostics().is_empty());
    assert_eq!(
        runtime
            .eval("marker = ''; inner.click();")
            .unwrap_err()
            .kind,
        rust_browser_javascript::ErrorKind::ExecutionLimit,
    );
    assert_eq!(runtime.eval("marker").unwrap(), Value::from(""));
}

#[test]
fn keyboard_event_construction_and_native_keydown_share_the_ui_event_interface() {
    let page = LoadedPage::from_html("https://events.test/", "<input id='target'>");
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    assert_eq!(runtime.eval(r#"
        var captured = "";
        const target = document.getElementById("target");
        target.addEventListener("keydown", event => {
            if (event instanceof KeyboardEvent && event instanceof UIEvent &&
                event.ctrlKey && event.getModifierState("Control")) {
                captured = event.key + "/" + event.code + "/" + event.isTrusted;
            }
        });
        const keyboard = new KeyboardEvent("keydown", {
            key: "c", code: "KeyC", ctrlKey: true, repeat: true, location: 3
        });
        target.dispatchEvent(keyboard);
        captured === "c/KeyC/false" && keyboard.repeat &&
            keyboard.location === KeyboardEvent.DOM_KEY_LOCATION_NUMPAD &&
            new KeyboardEvent("keydown").key === "" &&
            document.createEvent("KeyboardEvent") instanceof KeyboardEvent
    "#).unwrap(), Value::Bool(true));
    runtime.focus_control(page.document.elements("input").next()).unwrap();
    runtime.form_keydown(rust_browser_engine::script::FormKey::ArrowLeft, false, true).unwrap();
    assert_eq!(runtime.eval("captured").unwrap(), Value::from("ArrowLeft/ArrowLeft/true"));
}

#[test]
fn focus_events_report_the_other_element_in_the_focus_transition() {
    let page = LoadedPage::from_html("https://events.test/", "<input id='left'><input id='right'>");
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    assert_eq!(runtime.eval(r#"
        const left = document.getElementById("left"), right = document.getElementById("right");
        const events = [];
        left.addEventListener("blur", e => events.push(e instanceof FocusEvent && e.relatedTarget === right));
        right.addEventListener("focus", e => events.push(e instanceof UIEvent && e.relatedTarget === left));
        left.focus(); right.focus();
        events.length === 2 && events.every(Boolean) &&
            new FocusEvent("blur", {relatedTarget: right}).relatedTarget === right &&
            new FocusEvent("focus").relatedTarget === null
    "#).unwrap(), Value::Bool(true));
}

#[test]
fn dom_listeners_share_a_bounded_document_quota_and_reuse_removed_slots() {
    let page = LoadedPage::from_html("https://events.test/listeners", "<body>");
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    runtime
        .eval(
            r#"
        var targets = [], calls = 0;
        function listen() { calls++; }
        function addTargets(count) {
            for (var i = 0; i < count; ++i) {
                var target = document.createElement('span');
                target.addEventListener('check', listen);
                targets.push(target);
            }
        }
    "#,
        )
        .unwrap();
    for _ in 0..32 {
        runtime.eval("addTargets(256)").unwrap();
    }
    let error = runtime.eval("addTargets(1)").unwrap_err();
    assert_eq!(error.kind, rust_browser_javascript::ErrorKind::HeapLimit);
    assert_eq!(
        runtime
            .eval(
                r#"
            targets[8191].addEventListener('check', listen);
            targets[8191].dispatchEvent(new Event('check'));
            targets[0].removeEventListener('check', listen);
            addTargets(1);
            targets[8192].dispatchEvent(new Event('check'));
            calls
        "#
            )
            .unwrap(),
        Value::from(2),
    );
}

#[test]
fn animation_event_uses_event_flags_and_immutable_animation_data() {
    let page = LoadedPage::from_html("https://events.test/animation", "<button></button>");
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    let result = runtime.eval(r#"
        const button = document.querySelector('button');
        const seen = [];
        document.onanimationend = event => seen.push(event);
        class CustomAnimation extends AnimationEvent {}
        const event = new CustomAnimation('animationend', {
            animationName: 'fade', elapsedTime: 1.25, pseudoElement: '::before',
            bubbles: true
        });
        button.dispatchEvent(event);
        let invalid = false;
        try { new AnimationEvent('animationend', {elapsedTime: Infinity}); }
        catch (e) { invalid = e instanceof TypeError; }
        const defaults = new AnimationEvent('animationstart');
        seen[0] === event && event instanceof AnimationEvent && event instanceof Event
            && event.animationName === 'fade' && event.elapsedTime === 1.25
            && event.pseudoElement === '::before' && !event.isTrusted && invalid
            && defaults.animationName === '' && defaults.elapsedTime === 0
            && defaults.pseudoElement === '';
    "#).unwrap();
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn a_single_dom_target_preserves_its_listener_limit() {
    let page = LoadedPage::from_html("https://events.test/listeners", "<button></button>");
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    runtime
        .eval(
            r#"
        var button = document.querySelector('button'), n = 0, observed = false;
        function listen() { observed = true; }
        function addBatch() {
            for (var i = 0; i < 128; i++) button.addEventListener('event' + n++, listen);
        }
    "#,
        )
        .unwrap();
    for _ in 0..32 {
        runtime.eval("addBatch()").unwrap();
    }
    let error = runtime
        .eval("button.addEventListener('extra', listen)")
        .unwrap_err();
    assert_eq!(error.kind, rust_browser_javascript::ErrorKind::HeapLimit);
    assert_eq!(
        runtime
            .eval(
                r#"
        button.removeEventListener('event1', listen);
        button.addEventListener('extra', listen);
        button.dispatchEvent(new Event('extra'));
        observed
    "#
            )
            .unwrap(),
        Value::Bool(true)
    );
}
