use rust_browser_engine::{LoadedPage, script::ScriptRuntime};
use rust_browser_javascript::Value;
use std::sync::Arc;

fn runtime() -> ScriptRuntime {
    let page = LoadedPage::from_html(
        "https://example.test/",
        "<!doctype html><style>#box{height:40px;padding:2px 3px;border:1px solid black}</style>
         <div id=box style='width:100px;'>Box</div>",
    );
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    runtime
        .eval(
            r#"
            var box = document.getElementById("box");
            // The visible-element path of jQuery 1.4.2's dimension reads:
            // offsetWidth guard, axis offset, forced computed padding/borders.
            function dimension(isWidth) {
                if (box.offsetWidth === 0) throw new Error("box must be visible");
                var value = isWidth ? box.offsetWidth : box.offsetHeight;
                var sides = isWidth ? ["left", "right"] : ["top", "bottom"];
                for (var i=0; i<sides.length; i++) {
                    var view = box.ownerDocument.defaultView;
                    value -= parseFloat(view.getComputedStyle(box, null)
                        .getPropertyValue("padding-" + sides[i]));
                    value -= parseFloat(view.getComputedStyle(box, null)
                        .getPropertyValue("border-" + sides[i] + "-width"));
                }
                return value;
            }
            function assertDimensions(expectedWidth) {
                if (dimension(true) !== expectedWidth || dimension(false) !== 40)
                    throw new Error("unexpected dimensions");
            }
            "#,
        )
        .unwrap();
    runtime
}

#[test]
fn dimension_reads_share_layout_until_a_real_inline_change() {
    let mut runtime = runtime();
    let painted = runtime.layout_for_viewport(runtime.viewport()).unwrap();
    runtime
        .eval("for (var i=0; i<8; i++) assertDimensions(100)")
        .unwrap();
    assert!(runtime.layout_is_current(&painted));

    runtime.eval("box.style.width = '108px'").unwrap();
    assert!(runtime.layout_needs_refresh());
    assert_eq!(
        runtime.eval("box.offsetWidth === 116").unwrap(),
        Value::Bool(true)
    );
    assert!(!runtime.layout_needs_refresh());
    assert!(!runtime.layout_is_current(&painted));
    let resized = runtime.layout_for_viewport(runtime.viewport()).unwrap();
    assert!(!Arc::ptr_eq(&painted, &resized));
    runtime
        .eval("for (var i=0; i<8; i++) assertDimensions(108)")
        .unwrap();
    assert!(runtime.layout_is_current(&resized));
}

#[test]
fn equal_final_inline_writes_keep_the_dimension_cache_current() {
    let mut runtime = runtime();
    let painted = runtime.layout_for_viewport(runtime.viewport()).unwrap();
    runtime.eval("box.style.width = '100px'").unwrap();
    assert!(!runtime.layout_needs_refresh());
    runtime
        .eval("for (var i=0; i<8; i++) { box.style.width = '100px'; assertDimensions(100); }")
        .unwrap();
    assert!(runtime.layout_is_current(&painted));
    assert!(Arc::ptr_eq(
        &painted,
        &runtime.layout_for_viewport(runtime.viewport()).unwrap()
    ));
}
