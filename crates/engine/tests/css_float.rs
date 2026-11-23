use rust_browser_engine::{LoadedPage, layout::Viewport, script::ScriptRuntime};
use rust_browser_javascript::Value;

#[test]
fn ordinary_floats_create_layout_boxes_and_clearance() {
    let mut page = LoadedPage::from_html(
        "https://example.test/",
        include_str!("../../../tools/fixtures/css-float-flow.html"),
    );
    let mut runtime = ScriptRuntime::for_page_with_viewport(
        &mut page,
        Viewport {
            width: 800.,
            height: 600.,
            ..Default::default()
        },
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        runtime
            .eval("document.getElementById('result').getAttribute('data-result')")
            .unwrap(),
        Value::from("pass"),
    );
}
