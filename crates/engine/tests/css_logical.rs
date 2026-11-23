use ring::digest::{SHA256, digest};
use rust_browser_engine::{LoadedPage, css, layout::Viewport, script::ScriptRuntime};
use rust_browser_javascript::Value;
use serde_json::{Value as Json, json};

fn runtime(source: &str) -> ScriptRuntime {
    let page = LoadedPage::from_html("https://example.test/", source);
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    runtime.set_viewport(Viewport {
        width: 800.,
        height: 600.,
        ..Default::default()
    });
    for script in page.scripts.iter() {
        if let Some(source) = &script.source {
            runtime.eval(source).unwrap();
        }
    }
    runtime
}

#[test]
fn channel_toolbar_geometry_and_live_cssom_match_chrome() {
    let mut runtime = runtime(include_str!("fixtures/css_logical.html"));
    let report = runtime
        .eval("document.getElementById('result').textContent")
        .unwrap();
    let report: Json =
        serde_json::from_str(&report.as_string().unwrap().to_string_lossy()).unwrap();
    assert_eq!(
        report,
        json!({
            "first": [[0, 0, 320, 120], [16, 8, 24, 24], [54, 8, 72, 24], [288, 8, 20, 20]],
            "computed": ["16px 14px", "60px", "5px 7px", "3px", "12px", "12px"],
            "updated": [[8, 8, 24, 24], [46, 8, 76, 24], [12, 8, 20, 20]],
            "live": ["8px", "9px 7px", "rtl", "12px", "12px"],
        })
    );
}

#[test]
fn physical_and_logical_declarations_share_priority_and_inherit_by_name() {
    let mut runtime = runtime(
        r#"<!doctype html>
        <style>
            #outer { margin-inline: 7px 19px; }
            #inner {
                direction: var(--dir);
                --dir: ltr;
                margin-right: 28px;
                margin-inline: var(--unset-edges, inherit);
                margin-inline-end: 11px !important;
                padding-inline: 2px 3px;
                padding-right: 4px;
            }
            #initial { all: initial; margin-inline-start: 6px; }
        </style>
        <div id="outer" dir="RTL">
            <div id="inner" style="margin-right: 52px"></div>
            <div id="initial"></div>
        </div>"#,
    );
    let result = runtime
        .eval(
            r#"
            const inner = getComputedStyle(document.getElementById('inner'));
            const initial = getComputedStyle(document.getElementById('initial'));
            inner.direction === 'ltr' && inner.marginLeft === '7px'
                && inner.marginRight === '11px' && inner.marginInline === '7px 11px'
                && inner.paddingInline === '2px 4px'
                && initial.direction === 'rtl' && initial.marginRight === '6px';
            "#,
        )
        .unwrap();
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn axis_shorthands_validate_as_a_whole() {
    assert!(css::supports_declaration("margin-inline", "auto -1rem"));
    assert!(css::supports_declaration(
        "padding-block",
        "calc(1em + 2px) 10%"
    ));
    assert!(css::supports_declaration("inset-inline", "0 auto"));
    assert!(css::supports_declaration("max-block-size", "none"));
    assert!(!css::supports_declaration("padding-inline", "10px -1px"));
    assert!(!css::supports_declaration("margin-block", "0 0 0"));
    assert!(!css::supports_declaration("inset-block", "0 inherit"));
    assert!(!css::supports_declaration("max-inline-size", "auto"));

    let mut runtime = runtime(
        r#"<!doctype html><style>
          .box { margin-inline-start: 2px; margin-inline: 7px bogus; }
          .box { padding-inline: 5px 6px; padding-inline: 7px -1px; }
          #bad { padding-inline: var(--invalid); --invalid: 7px -1px; }
        </style>
        <div id="valid" class="box"></div><div id="bad" class="box"></div>"#,
    );
    assert_eq!(
        runtime
            .eval(
                "const valid = getComputedStyle(document.getElementById('valid'));\
                 valid.marginInline === '2px 0px' && valid.paddingInline === '5px 6px'\
                    && getComputedStyle(document.getElementById('bad')).paddingInline === '0px';",
            )
            .unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn specified_axis_shorthands_follow_single_edge_writes_and_removals() {
    let mut runtime = runtime(r#"<!doctype html><div id="box"></div>"#);
    runtime
        .eval(
            r#"
            const style = document.getElementById('box').style;
            const axes = [
                ['marginBlock', 'margin-block'], ['marginInline', 'margin-inline'],
                ['paddingBlock', 'padding-block'], ['paddingInline', 'padding-inline'],
                ['insetBlock', 'inset-block'], ['insetInline', 'inset-inline']
            ];
            function check(value, message) {
                if (!value) throw new Error(message);
            }
            for (const axis of axes) {
                const idl = axis[0], css = axis[1];
                const start = css + '-start', end = css + '-end';
                style[idl] = '4PX 10.0px';
                check(style[idl + 'Start'] === '4px' && style[end] === '10px', css + ' expands');
                style[idl + 'End'] = '12px';
                check(style[idl] === '4px 12px', css + ' recombines');
                check(style.removeProperty(start) === '4px', css + ' returns the old edge');
                check(style[idl] === '' && style[end] === '12px', css + ' keeps the other edge');
                check(style.removeProperty(css) === '' && style[end] === '', css + ' removes all edges');
                style.setProperty(start, '0');
                style[end] = '0.0px';
                check(style[idl] === '0px', css + ' elides equal values');
                check(style.removeProperty(css) === '0px', css + ' returns the old shorthand');
            }
            style.marginInline = 'inherit';
            style.marginInlineEnd = 'unset';
            check(style.marginInline === '' && style.marginInlineStart === 'inherit', 'mixed CSS-wide keywords');
            style.marginInline = null;
            check(style.marginInlineStart === '' && style.marginInlineEnd === '', 'null removes the axis');
            style.setProperty('--edges', '3px 5px');
            style.setProperty('margin-inline', 'var(--edges)', 'important');
            style.setProperty('margin-inline-end', '12px', 'important');
            const computed = getComputedStyle(document.getElementById('box'));
            check(style.marginInline === '' && style.marginInlineStart === ''
                && style.getPropertyPriority('margin-inline-start') === 'important'
                && computed.marginInline === '3px 12px', 'an override preserves a pending compatible edge');
            "#,
        )
        .unwrap();
}

#[test]
fn editing_logical_edges_preserves_priority_and_physical_source_order() {
    let mut runtime = runtime(
        r#"<!doctype html><div id="box" style="margin-inline-start:2px!important;
        margin-inline:4px 10px; margin-right:20px; color:red"></div>"#,
    );
    runtime
        .eval(
            r#"
            const box = document.getElementById('box');
            const style = box.style;
            const computed = getComputedStyle(box);
            function check(value, message) {
                if (!value) throw new Error(message);
            }
            check(style.marginInline === '' && style.marginInlineStart === '2px'
                && style.marginInlineEnd === '10px', 'authored !important picks each edge separately');
            check(style.getPropertyPriority('MARGIN-INLINE-START') === 'important'
                && style.getPropertyPriority('margin-inline') === '', 'authored priority');
            style.marginInlineStart = '8px';
            check(style.marginInline === '8px 10px' && computed.marginRight === '20px',
                'editing one edge preserves the other edge before a physical declaration');
            style.setProperty('MARGIN-INLINE', '4px 10px', 'IMPORTANT');
            check(style.marginInline === '4px 10px'
                && style.getPropertyPriority('margin-inline') === 'important', 'important shorthand');
            style.marginInlineEnd = '12px';
            check(style.marginInline === '' && style.marginInlineStart === '4px'
                && style.marginInlineEnd === '12px' && computed.marginRight === '12px',
                'one edge can replace its old important value');
            style.setProperty('margin-inline-start', '99px', ' important ');
            style.setProperty('margin-inline-start', '99px !important', 'important');
            check(style.marginInlineStart === '4px', 'invalid priority and inline bang do not set');
            style.setProperty('margin-inline', '6px 11px');
            check(style.marginInline === '6px 11px' && computed.marginRight === '11px'
                && style.getPropertyPriority('margin-inline-start') === '', 'the shorthand replaces both priorities');
            check(style.removeProperty('margin-inline') === '6px 11px'
                && computed.marginRight === '20px' && style.color === 'red', 'removal keeps unrelated declarations');
            const empty = document.createElement('div');
            check(empty.style.removeProperty('margin-inline') === '' && !empty.hasAttribute('style'),
                'removing an absent edge does not create a style attribute');
            "#,
        )
        .unwrap();
}

#[test]
fn pinned_wpt_logical_properties_with_variables() {
    // css/css-logical/logicalprops-with-variables.html at WPT revision
    // 9e1907a13cbbc338975302097a4bafcbc4d12dc5. License: fixtures/wpt-url/LICENSE.md.
    let fixture = include_str!("fixtures/wpt-logical/logicalprops-with-variables.html");
    let sha256: String = digest(&SHA256, fixture.as_bytes())
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert_eq!(
        sha256,
        "b248864d68b4c707bd0dc274ed11c7f97ad7041a8bc80776d57a9c4f20bae30c"
    );
    let page = LoadedPage::from_html("https://wpt.example/css/css-logical/", fixture);
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    runtime
        .eval(
            r#"
            let logicalWptPassed = 0;
            function test(run, name) {
                try { run(); }
                catch (error) { throw new Error(name + ': ' + error.message); }
                logicalWptPassed++;
            }
            function assert_equals(actual, expected, description) {
                if (!Object.is(actual, expected)) {
                    throw new Error(description + ': expected [' + expected + '], got [' + actual + ']');
                }
            }
            "#,
        )
        .unwrap();
    for script in page
        .scripts
        .iter()
        .filter_map(|script| script.source.as_ref())
    {
        runtime.eval(script).unwrap();
    }
    assert_eq!(
        runtime.eval("logicalWptPassed").unwrap(),
        Value::Number(12.)
    );
}
