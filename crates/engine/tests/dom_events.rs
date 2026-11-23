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
