use rust_browser_engine::{LoadedPage, script::ScriptRuntime};
use rust_browser_javascript::Value;

fn check(source: &str) {
    let page = LoadedPage::from_html("https://example.test/", "<!doctype html><body>");
    let mut r = ScriptRuntime::new(&page).unwrap();
    assert_eq!(r.eval(source).unwrap(), Value::Bool(true));
}

#[test]
fn legacy_jquery_css_support_probe_uses_real_inline_declarations() {
    check(
        r#"
        var d = document.createElement("div");
        d.innerHTML = "<a href='/a' style='color:red;float:left;opacity:.55;'>a</a>";
        var j = d.getElementsByTagName("a")[0];
        // Both support checks from the published jQuery 1.4.2.
        /^0.55$/.test(j.style.opacity) && !!j.style.cssFloat &&
            j.style.opacity === "0.55" && j.style.cssFloat === "left";
        "#,
    );
}

#[test]
fn css_float_aliases_and_edits_the_float_declaration() {
    check(
        r#"
        var node = document.createElement("div");
        var style = node.style;
        var originallyEmpty = style.cssFloat === "" && style["float"] === "";
        style.cssFloat = "right";
        var fromAlias = style["float"] === "right" &&
            style.getPropertyValue("float") === "right" &&
            style.getPropertyValue("cssFloat") === "" &&
            node.getAttribute("style").indexOf("right") >= 0;
        style.setProperty("FLOAT", "INLINE-END", "important");
        style.cssFloat = "middle";
        var fromMethod = style.cssFloat === "inline-end" &&
            style.getPropertyPriority("float") === "important";
        style["float"] = "LEFT";
        var fromNamedProperty = style.cssFloat === "left" &&
            style.getPropertyPriority("float") === "";
        style.cssFloat = null;
        originallyEmpty && fromAlias && fromMethod && fromNamedProperty &&
            style.getPropertyValue("float") === "";
        "#,
    );
}

#[test]
fn inline_opacity_serializes_specified_numbers_and_preserves_valid_values() {
    check(
        r#"
        var node = document.createElement("div");
        var style = node.style;
        node.setAttribute("style", "opacity:.7!important;opacity:.55;opacity:bad!important");
        var authored = style.opacity === "0.7" && style.getPropertyPriority("opacity") === "important";
        style.opacity = "50%";
        var percentage = style.opacity === "0.5" &&
            style.getPropertyValue("opacity") === "0.5" &&
            style.getPropertyPriority("opacity") === "";
        style.setProperty("opacity", "1.25", "important");
        style.opacity = "1.";
        style.opacity = "0.55px";
        style.opacity = "0.55;bad";
        var preserved = style.opacity === "1.25" &&
            style.getPropertyPriority("opacity") === "important";
        var removed = style.removeProperty("opacity") === "1.25" && style.opacity === "";
        style.opacity = "var(--opacity)";
        var variable = style.getPropertyValue("opacity") === "var(--opacity)";
        style.opacity = "inherit";
        authored && percentage && preserved && removed && variable && style.opacity === "inherit";
        "#,
    );
}

#[test]
fn padding_left_enables_the_jquery_ready_box_model_probe() {
    check(
        r#"
        var s = document;
        var k = s.createElement("div");
        var initiallyEmpty = k.style.paddingLeft === "";
        // Chained assignment and geometry check from the published ready callback.
        k.style.width = k.style.paddingLeft = "1px";
        s.body.appendChild(k);
        var boxModel = k.offsetWidth === 2;
        var reflected = k.style.getPropertyValue("padding-left") === "1px" &&
            k.style.getPropertyValue("width") === "1px";
        s.body.removeChild(k).style.display = "none";
        initiallyEmpty && boxModel && reflected;
        "#,
    );
}

#[test]
fn padding_left_reflects_the_inline_longhand() {
    check(
        r#"
        var node = document.createElement("div");
        var style = node.style;
        node.setAttribute("style", "padding-left:7px");
        var authored = style.paddingLeft === "7px" && style["padding-left"] === "7px";
        style["padding-left"] = "3px";
        style.paddingLeft = "-1px";
        var edited = style.paddingLeft === "3px" &&
            style.getPropertyValue("padding-left") === "3px";
        style.setProperty("padding-left", "5px", "important");
        var fromMethod = style.paddingLeft === "5px" &&
            style.getPropertyPriority("padding-left") === "important";
        var removed = style.removeProperty("padding-left") === "5px" && style.paddingLeft === "";
        authored && edited && fromMethod && removed;
        "#,
    );
}
