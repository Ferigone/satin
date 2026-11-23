use rust_browser_engine::{
    LoadedPage,
    history::{HistoryScroll, NavigationMode, ScriptNavigation, SessionHistory},
    layout::ScrollOffset,
    script::ScriptRuntime,
};
use std::sync::Arc;
use url::Url;

#[test]
fn window_and_location_reject_private_elements_and_ordinary_receivers_accept_them() {
    let page = LoadedPage::from_html("https://example.test/", "");
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    let result = runtime
        .eval(
            r#"
            class Target { constructor(o) { return o; } }
            class Field extends Target {
                #value = 7;
                static read(o) { return o.#value; }
            }
            class Method extends Target {
                #value() { return 8; }
                static read(o) { return o.#value(); }
            }
            const types = [];
            for (const receiver of [window, document.location]) {
                for (const Stamp of [Field, Method]) {
                    try { new Stamp(receiver); }
                    catch (error) { types.push(error instanceof TypeError); }
                }
            }
            const ordinary = Object.preventExtensions({});
            new Field(ordinary);
            new Method(ordinary);
            types.join() === 'true,true,true,true'
                && Field.read(ordinary) === 7 && Method.read(ordinary) === 8
        "#,
        )
        .unwrap();
    assert_eq!(result.as_bool(), Some(true));
}

#[test]
fn history_rewrites_live_addresses_and_serializes_identity_without_copying_the_dom() {
    let mut page = LoadedPage::from_html(
        "https://example.test/old/index.html?first",
        "<title>App</title><form id='submit' action='submit'></form><p>Content</p>",
    );
    let document = page.document.clone();
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    let result = runtime
        .eval(
            r#"
        const input = { next: 3, text: '\ud800', nan: NaN, negative: -0, holes: new Array(2) };
        input.self = input;
        input.other = input.holes;
        input.holes[1] = undefined;
        const initial = history.length;
        history.pushState(input, '', '/new/route?mode=2#section');
        input.next = 17;
        const restored = history.state;
        history.length === initial+1 && history.state === restored && restored !== input
          && restored.self === restored && restored.holes === restored.other
          && !(0 in restored.holes) && 1 in restored.holes && restored.holes.length === 2
          && restored.nan !== restored.nan && 1/restored.negative === -Infinity
          && restored.text.charCodeAt(0) === 55296 && restored.next === 3
          && location === window.location && window.location === document.location
          && Object.getOwnPropertyDescriptor(location,'href').configurable === false
          && document.URL === location.href && document.documentURI === location.href
          && document.baseURI === 'https://example.test/new/route?mode=2#section'
          && location.pathname === '/new/route' && location.search === '?mode=2'
          && location.origin === 'https://example.test' && location.host === 'example.test'
          && document.getElementById('submit').action === 'https://example.test/new/submit'
          && new Request('fetch').url === 'https://example.test/new/fetch'
    "#,
        )
        .unwrap();
    assert_eq!(result.as_bool(), Some(true));
    assert!(runtime.snapshot(&mut page));
    assert!(Arc::ptr_eq(&page.document, &document));
    assert_eq!(page.url, "https://example.test/new/route?mode=2#section");
    let errors = runtime.eval(r#"
        var failures = [];
        const before = history.state;
        const length = history.length;
        try { history.pushState({ nested: () => 1 }, '', 'https://other.test/'); } catch(e) { failures.push(e.name); }
        try { history.pushState(document.body, '', '#rejected'); } catch(e) { failures.push(e.name); }
        try { history.pushState(1, '', 'https://user@example.test/new/route'); } catch(e) { failures.push(e.name); }
        try { history.pushState(1, '', 'http://example.test/new/route'); } catch(e) { failures.push(e.name); }
        failures.push(history.state === before && history.length === length);
        failures.join(',')
    "#).unwrap();
    assert_eq!(
        errors.as_string().unwrap().to_string_lossy(),
        "DataCloneError,DataCloneError,SecurityError,SecurityError,true"
    );
}

#[test]
fn hash_updates_are_synchronous_and_navigation_requests_are_deferred() {
    let page = LoadedPage::from_html("https://example.test/start", "<p id='two'>Two</p>");
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    let result = runtime
        .eval(
            r#"
        var trace = [];
        window.onhashchange = event => trace.push(event.oldURL + '→' + event.newURL);
        Promise.resolve().then(() => trace.push('microtask'));
        location.hash = '#two';
        trace.push(location.hash);
        location.hash = '#two';
        trace.join('|')
    "#,
        )
        .unwrap();
    assert_eq!(result.as_string().unwrap().to_string_lossy(), "#two");
    assert!(runtime.has_navigation_tasks());
    runtime.pump();
    let observed = runtime.eval("trace.join('|')").unwrap();
    assert_eq!(
        observed.as_string().unwrap().to_string_lossy(),
        "#two|microtask|https://example.test/start→https://example.test/start#two"
    );
    runtime
        .eval("history.go(-2); location.assign('/next');")
        .unwrap();
    assert!(matches!(
        runtime.take_navigation_request(),
        Some(ScriptNavigation::Traverse(-2))
    ));
    assert!(
        matches!(runtime.take_navigation_request(), Some(ScriptNavigation::Navigate { url, .. }) if url.as_str()=="https://example.test/next")
    );
    assert!(!runtime.has_navigation_tasks());
}

#[test]
fn navigation_traversal_distinguishes_fragment_entries_and_documents() {
    let address = Url::parse("https://example.test/index").unwrap();
    let history = SessionHistory::new(address.clone());
    let first_scroll = ScrollOffset { x: 0., y: 88. };
    let target = address.join("#one").unwrap();
    assert!(
        history
            .fragment(target.clone(), NavigationMode::Push, first_scroll)
            .unwrap()
            .same_document
    );
    assert!(history.can_traverse(-1));
    let transition = history.traverse(-1, ScrollOffset::default()).unwrap();
    assert!(transition.same_document);
    assert_eq!(transition.url, address);
    assert!(matches!(transition.scroll, HistoryScroll::Restore(offset) if offset == first_scroll));
    assert!(history.traverse(1, first_scroll).unwrap().same_document);
    let second = target.join("page").unwrap();
    history.visit(second, NavigationMode::Push, ScrollOffset::default());
    assert!(
        !history
            .traverse(-1, ScrollOffset::default())
            .unwrap()
            .same_document
    );
    history.visit(
        target.join("replaced").unwrap(),
        NavigationMode::Replace,
        ScrollOffset::default(),
    );
    assert!(history.can_traverse(1));
}

#[test]
fn redirecting_a_restored_entry_breaks_its_same_document_group() {
    let address = Url::parse("https://example.test/index").unwrap();
    let history = SessionHistory::new(address.clone());
    let scroll = ScrollOffset::default();
    history
        .fragment(
            address.join("#second").unwrap(),
            NavigationMode::Push,
            scroll,
        )
        .unwrap();
    history.visit(
        Url::parse("https://other.test/current").unwrap(),
        NavigationMode::Push,
        scroll,
    );
    assert!(!history.traverse(-1, scroll).unwrap().same_document);
    history.commit_address(Url::parse("https://redirect.test/destination").unwrap());
    let next = history.traverse(-1, scroll).unwrap();
    assert!(!next.same_document);
    assert_eq!(next.url, address);
}
