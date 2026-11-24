use rust_browser_engine::{LoadedPage, script::ScriptRuntime};
use rust_browser_javascript::Value;

#[test]
fn same_live_view_tracks_new_indices_named_items_and_removal() {
    let page = LoadedPage::from_html(
        "https://example.test",
        "<!doctype html><main><p id=a class='card hot'>A</p><p id=b class='card'>B</p></main>",
    );
    let mut realm = ScriptRuntime::new(&page).unwrap();
    let result = realm
        .eval(
            r#"
        var main = document.querySelector('main');
        var tags = main.getElementsByTagName('P');
        var cards = document.getElementsByClassName('card hot');
        var children = main.children;
        var nodes = main.childNodes;
        var old = tags[0];
        var p = document.createElement('p');
        p.id = 'c';
        p.setAttribute('class', 'hot card card');
        main.appendChild(p);
        old.remove();
        tags instanceof HTMLCollection && nodes instanceof NodeList &&
        main.children === children && main.childNodes === nodes &&
        tags.length === 2 && tags[0].id === 'b' && tags[1] === p &&
        !(2 in tags) && tags[2] === undefined && tags.item(2) === null &&
        children.namedItem('c') === p && nodes.item(1) === p &&
        Object.keys(tags).join(',') === '0,1' &&
        Object.getOwnPropertyDescriptor(tags,'1').value === p &&
        cards.length === 1 && cards[0] === p && old.parentNode === null
    "#,
        )
        .unwrap();
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn tag_names_match_html_case_and_preserve_foreign_case_and_template_inertness() {
    let page = LoadedPage::from_html(
        "https://example.test",
        "<!doctype html><svg><linearGradient id=a></linearGradient></svg><template><svg><linearGradient id=b/></svg></template><input name=group>",
    );
    let mut realm = ScriptRuntime::new(&page).unwrap();
    let result = realm.eval(r#"
        var found = document.getElementsByTagName('linearGradient');
        var lower = document.getElementsByTagName('lineargradient');
        var names = document.getElementsByName('group');
        found.length === 1 && found[0].id === 'a' && lower.length === 0 &&
        document.getElementsByTagNameNS('http://www.w3.org/2000/svg','linearGradient')[0] === found[0] &&
        document.getElementsByTagName('INPUT')[0] === names.item(0) &&
        Array.from(found)[0] === found[0]
    "#).unwrap();
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn query_selector_all_returns_ordered_static_node_lists() {
    let page = LoadedPage::from_html(
        "https://example.test",
        "<!doctype html><main><p id=a class=row>A</p><!-- comment --><p id=b class=row>B</p></main><p id=outside class=row></p>",
    );
    let mut realm = ScriptRuntime::new(&page).unwrap();
    let result = realm
        .eval(
            r#"
        var main = document.querySelector('main');
        var list = main.querySelectorAll('#b, .row');
        var a = main.firstChild;
        var b = main.lastChild;
        var replacement = a.cloneNode(true);
        a.remove();
        main.appendChild(replacement);
        var current = main.querySelectorAll('.row');
        var sliced = Array.prototype.slice.call(list);
        list instanceof NodeList && !Array.isArray(list) &&
        list.length === 2 && list[0] === a && list.item(1) === b &&
        list[2] === undefined && list.item(2) === null &&
        sliced.length === 2 && sliced[0] === a && sliced[1] === b &&
        current[0] === b && current[1] === replacement && current !== list &&
        main.querySelectorAll('span') instanceof NodeList
    "#,
        )
        .unwrap();
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn invalid_dom_selector_list_throws_before_any_member_can_match() {
    let page = LoadedPage::from_html(
        "https://example.test",
        "<!doctype html><main><p class=row>A</p><p class=row>B</p></main>",
    );
    let mut realm = ScriptRuntime::new(&page).unwrap();
    let result = realm
        .eval(
            r#"
        var main = document.querySelector('main');
        var first = main.firstChild;
        function syntaxError(receiver, method, selector) {
            try {
                receiver[method](selector);
                return false;
            } catch (e) {
                return e instanceof DOMException && e.name === 'SyntaxError';
            }
        }
        document.querySelectorAll('main > .row:nth-child(2)')[0] === main.lastChild &&
        syntaxError(document, 'querySelectorAll', '.row, .row:eq(1)') &&
        syntaxError(document, 'querySelectorAll', 'main > .row:visible') &&
        syntaxError(main, 'querySelectorAll', '.row:eq(1)') &&
        syntaxError(document.createDocumentFragment(), 'querySelectorAll', '.row,') &&
        syntaxError(document, 'querySelector', '.row, [') &&
        syntaxError(first, 'matches', '.row, :unknown') &&
        syntaxError(first, 'closest', '.row, :unknown')
    "#,
        )
        .unwrap();
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn repeated_fragment_views_keep_identity_and_charge_only_the_searched_subtree() {
    let page = LoadedPage::from_html("https://example.test", "<body><main></main></body>");
    let mut realm = ScriptRuntime::new(&page).unwrap();
    realm
        .eval(
            r#"
        var holder = document.createDocumentFragment();
        var list = holder.childNodes, elementList = holder.children;
        list.tag = 'preserved';
        var main = document.getElementsByTagName('main')[0];
        function updateViews() {
            for (var i = 0; i < 96; i++) {
                var fragment = document.createDocumentFragment();
                var child = document.createTextNode('value');
                fragment.appendChild(child);
                if (fragment.childNodes[0] !== child) throw Error('lost child');
                main.replaceChildren(fragment);
            }
        }
    "#,
        )
        .unwrap();
    // More than the former lifetime cap of 8192 temporary owner views.
    for _ in 0..87 {
        realm.eval("updateViews()").unwrap();
        realm.pump_with_network_errors().unwrap();
    }
    assert_eq!(
        realm
            .eval(
                r#"
        holder.appendChild(document.createElement('span'));
        var found = holder.getElementsByTagName('span');
        for (var i = 0; i < 80; i++) {
            main.firstChild.data = String(i);
            if (found.length !== 1) throw Error('incorrect live subtree');
        }
        list === holder.childNodes && elementList === holder.children &&
            list.tag === 'preserved' && list[0] === elementList[0] &&
            list[0] === found[0] && main.childNodes.length === 1
    "#
            )
            .unwrap(),
        Value::Bool(true)
    );
}
