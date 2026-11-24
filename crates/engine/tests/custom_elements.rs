use rust_browser_engine::{LoadedPage, script::ScriptRuntime};
use rust_browser_javascript::Value;

fn runtime(html: &str) -> ScriptRuntime {
    ScriptRuntime::new(&LoadedPage::from_html("https://example.test/", html)).unwrap()
}

#[test]
fn definition_upgrades_the_existing_html_wrapper_and_observed_attributes() {
    let mut runtime = runtime(
        "<body><x-panel data-state='open'></x-panel><svg><x-panel></x-panel></svg><template><x-panel></x-panel></template>",
    );
    let actual = runtime.eval(r#"
        const before = document.querySelector('x-panel');
        const log = [];
        class Base extends HTMLElement { constructor() { super(); this.initialized = true; } }
        class Panel extends Base {
            static get observedAttributes() { return ['data-state']; }
            constructor() { super(); log.push('new'); }
            connectedCallback() { log.push('connect'); }
            attributeChangedCallback(name, old, value, ns) { log.push([name,old,value,ns].join(':')); }
        }
        customElements.define('x-panel', Panel);
        before.setAttribute('data-state', 'closed');
        before.removeAttribute('data-state');
        [before === document.querySelector('x-panel'), before instanceof Panel, before.initialized,
         document.querySelector('svg').firstChild instanceof Panel,
         document.querySelector('template').content.firstChild instanceof Panel,
         customElements.get('x-panel') === Panel, log.join(',')].join('|');
    "#).unwrap();
    assert_eq!(
        actual,
        Value::from(
            "true|true|true|false|false|true|new,data-state::open:,connect,data-state:open:closed:,data-state:closed::"
        )
    );
}

#[test]
fn new_and_create_element_get_dom_identity_and_connection_callbacks() {
    let mut runtime = runtime("<body><section></section></body>");
    let actual = runtime
        .eval(
            r#"
        const log = [];
        class Component extends HTMLElement {
            constructor() { super(); this.name = this.nodeName; }
            connectedCallback() { log.push('in:' + this.id); }
            disconnectedCallback() { log.push('out:' + this.id); }
        }
        const old = document.createElement('x-cell');
        customElements.define('x-cell', Component);
        const first = new Component(), second = document.createElement('x-cell');
        first.id = 'a'; second.id = 'b'; old.id = 'c';
        const section = document.querySelector('section');
        section.appendChild(first);
        section.appendChild(second);
        section.appendChild(old);
        document.body.appendChild(second);
        section.remove();
        [first instanceof Component, first instanceof HTMLElement, first instanceof Node,
         first.name, second.name, old.name, log.join(',')].join('|');
    "#,
        )
        .unwrap();
    assert_eq!(
        actual,
        Value::from("true|true|true|X-CELL|X-CELL|X-CELL|in:a,in:b,in:c,out:b,in:b,out:a,out:c")
    );
}

#[test]
fn definition_validation_and_promises_use_the_registered_constructor() {
    let mut runtime = runtime("<body></body>");
    let actual=runtime.eval(r#"
        const events = [];
        const pending = customElements.whenDefined('x-test');
        const same = pending === customElements.whenDefined('x-test');
        class X extends HTMLElement {}
        function fails(f) { try { f(); } catch(e) { return e.name; } return ''; }
        pending.then(ctor => events.push(ctor === X));
        customElements.whenDefined('invalid').catch(e => events.push(e.name));
        customElements.define('x-test', X);
        customElements.whenDefined('x-test').then(ctor => events.push(ctor === X));
        [same, customElements instanceof CustomElementRegistry, customElements.get('unregistered') === undefined,
         fails(() => new HTMLElement()), fails(() => new (class extends HTMLElement {})()),
         fails(() => customElements.define('invalid', class {})),
         fails(() => customElements.define('x-also', X)),
         fails(() => customElements.define('x-test', class {})),
         fails(() => customElements.define('x-invalid', () => 1))].join('|');
    "#).unwrap();
    assert_eq!(
        actual,
        Value::from(
            "true|true|true|TypeError|TypeError|SyntaxError|NotSupportedError|NotSupportedError|TypeError"
        )
    );
    runtime.pump();
    assert_eq!(
        runtime.eval("events.join('|')").unwrap(),
        Value::from("SyntaxError|true|true")
    );
}

#[test]
fn failed_upgrades_do_not_repeat_and_later_candidates_still_run() {
    let mut runtime = runtime("<body><x-once id=bad></x-once><x-once id=ok></x-once></body>");
    let actual=runtime.eval(r#"
        const log = [];
        class Once extends HTMLElement {
            constructor(){super(); log.push(this.id); if(this.id==='bad') throw new Error('fixture failure');}
            connectedCallback(){log.push('connected:'+this.id);}
        }
        customElements.define('x-once',Once);
        const bad = document.getElementById('bad');
        bad.remove();
        document.body.appendChild(bad);
        class Invalid extends HTMLElement {constructor(){super(); this.id = 'disallowed';} }
        customElements.define('x-invalid',Invalid);
        const failed=document.createElement('x-invalid');
        [log.join(','), failed instanceof Invalid, failed.id, failed.nodeName].join('|');
    "#).unwrap();
    assert_eq!(actual, Value::from("bad,ok,connected:ok|false||X-INVALID"));
}
