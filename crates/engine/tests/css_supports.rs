use rust_browser_engine::{LoadedPage, script::ScriptRuntime};
use rust_browser_javascript::Value;

fn check(runtime: &mut ScriptRuntime, source: &str) {
    assert_eq!(runtime.eval(source).unwrap(), Value::Bool(true), "{source}");
}

#[test]
fn supports_is_a_window_namespace_operation_with_webidl_overloads() {
    let page = LoadedPage::from_html("https://satin.test/", "");
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    for expression in [
        "CSS === window.CSS && Object.prototype.toString.call(CSS) === '[object CSS]'",
        "!Object.getOwnPropertyDescriptor(window, 'CSS').enumerable && CSS.supports.length === 1",
        "CSS.supports('display', 'grid') && CSS.supports('DISPLAY', 'flex')",
        "CSS.supports.call(null, 'color', 'red', 'ignored')",
        "CSS.supports('--X','x:y') && CSS.supports('--X', ' ') && !CSS.supports('--X', '')",
        "!CSS.supports('color ', 'red') && !CSS.supports('width', '3px;')",
        "!CSS.supports('width', '-5px') && !CSS.supports('color', 'red !important')",
        "!CSS.supports('nonexistent', 'inherit') && !CSS.supports('color', 'var(broken)')",
        "CSS.supports('color: red !important') && CSS.supports('(--x: )')",
        "CSS.supports('(color:red) and (display:flex)') && CSS.supports('not (unrecognized)')",
        "CSS.supports('((color:green) or (width:wrong)) and (not (height:wrong))')",
        "!CSS.supports('(width:1px) and (color:red) or (height:2px)')",
        "!CSS.supports('not (color:red) trailing') && !CSS.supports('not (width:[]]') && !CSS.supports('not 12()')",
        "CSS.supports('selector(main:has(> :where(.lead)))')",
        "CSS.supports('font-format(WOFF2)') && !CSS.supports('font-format(woff2-variations)')",
        "CSS.supports('font-tech(features-opentype)') && !CSS.supports('font-tech(color-COLRv1)')",
        "!CSS.supports('selector(div, span)') && !CSS.supports('selector(:is(div, :unknown))')",
        "var order = ''; CSS.supports({toString(){order += 'p';return 'color'}},{toString(){order += 'v';return 'blue'}}) && order === 'pv'",
        "var missing = false; try { CSS.supports() } catch(e) { missing = e instanceof TypeError }; missing",
        "var throws = false; try { CSS.supports({toString(){throw 21}}) } catch(e) { throws = e === 21 }; throws",
    ] {
        check(&mut runtime, expression);
    }
}

#[test]
fn cssom_feature_tests_and_conditional_styles_use_the_same_parser() {
    let page = LoadedPage::from_html(
        "https://satin.test/",
        r#"
        <style>
          #box { width: 30px; height: 12px }
          @supports (display: grid) and selector(section:has(> i)) {
            @media (min-width: 300px) { #box { width: 84px } }
          }
          @supports not (width: nonsense) { #box { height: 24px } }
          @supports (not-implemented: 1) { #box { width: 200px } }
          @supports selector(:where(i, :unknown)) { #box { height: 200px } }
          @supportsInvalid (color: green) { #box { width: 999px } }
        </style><section id="box"><i></i></section>
    "#,
    );
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    check(
        &mut runtime,
        r#"
        var box = document.querySelector('#box');
        CSS.supports('(display: grid) and selector(section:has(> i))') &&
        box.getBoundingClientRect().width === 84 && box.getBoundingClientRect().height === 24
    "#,
    );
}

#[test]
fn conditional_font_faces_and_font_sources_agree_on_supported_technologies() {
    use rust_browser_engine::{
        css,
        fonts::{FontFormat, FontSource},
    };
    let rules = css::font_faces(
        r#"
          @supports font-tech(features-opentype) and font-format(woff2) {
            @font-face { font-family: Active; src: url(active.woff2) format(woff2) tech(variations, features-opentype) }
          }
          @supports font-tech(unimplemented) {
            @font-face { font-family: Inactive; src: url(inactive.woff2) }
          }"#,
        800.,
    );
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].family.as_ref(), "Active");
    assert!(matches!(
        &rules[0].sources[..],
        [FontSource::Url {
            format: Some(FontFormat::Woff2),
            ..
        }]
    ));
}
