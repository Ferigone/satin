use rust_browser_engine::{LoadedPage, script::ScriptRuntime};
use rust_browser_javascript::Value;

#[test]
fn constructed_rules_are_live_and_retain_identity() {
    let page = LoadedPage::from_html("https://example.test/", "");
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    assert_eq!(
        runtime
            .eval(
                r#"
        const sheet = new CSSStyleSheet(), list = sheet.cssRules;
        sheet.replaceSync('@import url("a.css"); h1, h2 { color: red }');
        const first = list[0];
        sheet.insertRule('p { display: block }', 1);
        const saved = Array.from(list).map(rule => rule.cssText).join('|');
        sheet.deleteRule(0);
        [sheet instanceof StyleSheet,list === sheet.cssRules,list[0] === list.item(0),
         list.item(1) === null,first.parentStyleSheet === null,
         list[0].parentStyleSheet === sheet,saved].join(',')
    "#
            )
            .unwrap(),
        Value::from("true,true,true,true,true,true,h1, h2 { color: red; }|p { display: block; }")
    );
}

#[test]
fn invalid_insert_preserves_rules_and_constructor_option_is_reflected() {
    let page = LoadedPage::from_html("https://example.test/", "");
    let mut rt = ScriptRuntime::new(&page).unwrap();
    assert_eq!(
        rt.eval(
            r#"
        const s=new CSSStyleSheet({disabled:true}), initial=s.disabled;
        s.disabled=false;
        s.insertRule('p {}');
        const failures=[];
        for(const text of ['p {} trailing-junk','p {} @import "x";','@import "x";']) {
          try {s.insertRule(text)} catch(e) {failures.push(e.name)}
        }
        [initial,s.disabled,s.cssRules.length,s.cssRules[0].cssText,failures].join('|')
    "#
        )
        .unwrap(),
        Value::from("true|false|1|p { }|SyntaxError,SyntaxError,SyntaxError")
    );
}
