use rust_browser_engine::{LoadedPage, script::ScriptRuntime};
use rust_browser_javascript::Value;

fn check(source: &str) {
    let page = LoadedPage::from_html(
        "https://example.test/",
        "<!doctype html><div id=a style='width:100px'></div><div id=b style='width:137px'></div>",
    );
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    assert_eq!(runtime.eval(source).unwrap(), Value::Bool(true));
}

#[test]
fn computed_get_property_value_checks_receiver_before_name_coercion() {
    check(
        r#"
        var style = getComputedStyle(document.getElementById("a"));
        var read = style.getPropertyValue;
        var conversions = 0, marker = new Error("name coerced");
        var propertyName = { toString: function () { conversions++; throw marker; } };
        function rejects(receiver) {
            try { read.call(receiver, propertyName); }
            catch (error) { return error.name === "TypeError" && conversions === 0; }
            return false;
        }
        var plain = rejects({});
        var inherited = rejects(Object.create(style));
        var propagated = false;
        try { read.call(style, propertyName); }
        catch (error) { propagated = error === marker; }
        plain && inherited && propagated && conversions === 1;
        "#,
    );
}

#[test]
fn extracted_computed_get_property_value_reads_its_live_receiver() {
    check(
        r#"
        var A = getComputedStyle(document.getElementById("a"));
        var b = document.getElementById("b");
        var B = getComputedStyle(b);
        var direct = A.getPropertyValue("width") === "100px" &&
            B.getPropertyValue("width") === "137px";
        var read = A.getPropertyValue;
        var borrowed = read.call(B, "width") === "137px";
        var updated = read.call(B, {
            toString: function () { b.style.width = "151px"; return "width"; }
        });
        direct && borrowed && updated === "151px" &&
            A.getPropertyValue("width") === "100px" && B.getPropertyValue("width") === "151px";
        "#,
    );
}
