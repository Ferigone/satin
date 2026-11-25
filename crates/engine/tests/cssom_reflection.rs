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

#[test]
fn computed_list_marker_tracks_inline_changes_on_the_list() {
    check(
        r#"
        var list = document.createElement("ol");
        var item = document.createElement("li");
        list.appendChild(item);
        document.body.appendChild(list);
        var computed = getComputedStyle(item);
        var initial = computed.display === "list-item" && computed.listStyleType === "decimal";
        list.style.listStyle = "none";
        var hidden = computed.listStyle === "outside none none" &&
            computed.getPropertyValue("list-style-type") === "none";
        item.style.listStyleType = "square";
        initial && hidden && computed.listStyleType === "square";
        "#,
    );
}

#[test]
fn computed_opacity_clamps_numbers_and_percentages_without_inheriting() {
    check(
        r#"
        var parent = document.createElement("div");
        var child = document.createElement("div");
        parent.appendChild(child);
        document.body.appendChild(parent);
        var style = getComputedStyle(parent);
        var initial = style.opacity === "1";
        parent.style.opacity = "25%";
        var quarter = style.getPropertyValue("opacity") === "0.25";
        parent.style.opacity = "-0.1";
        var zero = style.opacity === "0" && getComputedStyle(child).opacity === "1";
        parent.style.opacity = "110%";
        initial && quarter && zero && style.opacity === "1";
    "#,
    );
}

#[test]
fn computed_flex_properties_follow_cascade_shorthands_and_individual_edits() {
    check(
        r#"
        var box = document.createElement("div");
        document.body.appendChild(box);
        var cs = getComputedStyle(box);
        var initial = cs.flex === "0 1 auto" && cs.flexFlow === "row nowrap" && cs.order === "0";
        box.setAttribute("style", "font-size:15px; flex:2 3 2em; flex-flow:wrap-reverse column-reverse; order:-2");
        var styled = cs.flexDirection === "column-reverse" && cs.flexWrap === "wrap-reverse" &&
            cs.getPropertyValue("flex-flow") === "column-reverse wrap-reverse" &&
            cs.flexGrow === "2" && cs.flexShrink === "3" && cs.flexBasis === "30px" &&
            cs.flex === "2 3 30px" && cs.order === "-2";
        box.style.setProperty("flex-basis", "calc(25% + 3px)");
        var updated = cs["flex-basis"] === "calc(25% + 3px)" && cs.flex === "2 3 calc(25% + 3px)";
        box.style.setProperty("flex", "1");
        initial && styled && updated && cs.flex === "1 1 0%";
    "#,
    );
}

#[test]
fn computed_border_colors_follow_each_edge_and_live_currentcolor() {
    check(
        r#"
        var box = document.createElement("div");
        document.body.appendChild(box);
        var cs = getComputedStyle(box);
        box.setAttribute("style", "color:blue; border-color:currentColor red green");
        var first = cs.borderTopColor === "rgb(0, 0, 255)" && cs.borderRightColor === "rgb(255, 0, 0)" &&
            cs.getPropertyValue("border-bottom-color") === "rgb(0, 128, 0)" &&
            cs["border-left-color"] === cs.borderRightColor &&
            cs.borderColor === "rgb(0, 0, 255) rgb(255, 0, 0) rgb(0, 128, 0)";
        box.style.color = "black";
        box.style.setProperty("border-left-color", "white");
        first && cs.borderColor === "rgb(0, 0, 0) rgb(255, 0, 0) rgb(0, 128, 0) rgb(255, 255, 255)";
    "#,
    );
}

#[test]
fn flex_and_border_named_writes_use_the_live_inline_declaration() {
    check(
        r#"
        var box = document.createElement("div");
        document.body.appendChild(box);
        var style = box.style;
        var cs = getComputedStyle(box);
        var empty = style.flexWrap === "" && style["flex-wrap"] === "";
        style["flex-wrap"] = "wrap-reverse";
        var reverse = style.flexWrap === "wrap-reverse" && cs.flexWrap === "wrap-reverse";
        style.setProperty("flex-wrap", "wrap", "important");
        style.flexWrap = "invalid-value";
        var preserved = style["flex-wrap"] === "wrap" && style.getPropertyPriority("flex-wrap") === "important";
        style.flexWrap = "nowrap";
        style.flexGrow = "2";
        style["flex-direction"] = "column";
        style.borderTopColor = "red";
        style["border-left-color"] = "blue";
        var edited = cs.getPropertyValue("flex-wrap") === "nowrap" && style.getPropertyPriority("flex-wrap") === "" &&
            cs.flexGrow === "2" && cs.flexDirection === "column" &&
            style["border-top-color"] === "red" && style.borderLeftColor === "blue" &&
            cs.borderTopColor === "rgb(255, 0, 0)" && cs.borderLeftColor === "rgb(0, 0, 255)";
        style["flex-wrap"] = null;
        empty && reverse && preserved && edited && style.flexWrap === "" && cs.flexWrap === "nowrap";
        "#,
    );
}
