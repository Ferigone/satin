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
