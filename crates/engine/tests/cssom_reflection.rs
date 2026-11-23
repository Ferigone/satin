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

#[test]
fn equal_custom_property_writes_preserve_case_and_notify_only_real_edits() {
    check(
        r#"
        var node = document.createElement("div");
        node.setAttribute("style", "--tone:blue;--Tone:Red;");
        var style = node.style;
        var observer = new MutationObserver(function () {});
        observer.observe(node, {attributes:true, attributeFilter:["style"]});
        var authored = node.getAttribute("style");
        style.setProperty("--Tone", "Red");
        var initial = observer.takeRecords().length === 0 && node.getAttribute("style") === authored;
        style.setProperty("--Tone", "Red", "important");
        var addedPriority = observer.takeRecords().length === 1 &&
            style.getPropertyPriority("--Tone") === "important";
        style.setProperty("--Tone", "Red", "IMPORTANT");
        var repeatedImportant = observer.takeRecords().length === 0;
        style.setProperty("--Tone", "Green", "important");
        var changed = observer.takeRecords().length === 1;
        var removed = style.removeProperty("--Tone") === "Green";
        initial && addedPriority && repeatedImportant && changed && removed &&
            observer.takeRecords().length === 1 && style.getPropertyValue("--tone") === "blue";
        "#,
    );
}

#[test]
fn float_aliases_do_not_skip_a_same_value_priority_change() {
    check(
        r#"
        var node = document.createElement("div");
        var style = node.style;
        style.setProperty("FLOAT", "right", "important");
        var observer = new MutationObserver(function () {});
        observer.observe(node, {attributes:true, attributeFilter:["style"]});
        style.cssFloat = "right";
        var changed = observer.takeRecords().length === 1 &&
            style.getPropertyPriority("float") === "";
        style["float"] = "RIGHT";
        changed && observer.takeRecords().length === 0 && style.cssFloat === "right";
        "#,
    );
}
