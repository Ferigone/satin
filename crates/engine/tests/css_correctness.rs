use rust_browser_engine::{
    css::{self, BoxSizing, Color, Display, Length, Style, Stylesheet},
    dom::{Document, NodeId},
    html,
};

fn styles(source: &str, sheet: &str) -> (Document, Vec<Style>) {
    let doc = html::parse(source);
    let rules = Stylesheet::new(css::parse(sheet, 800.), 800.);
    let mut computed = vec![Style::default(); doc.nodes.len()];
    let mut stack = vec![doc.root];
    let mut root_font = 16.;
    while let Some(id) = stack.pop() {
        let parent = doc.nodes[id.0]
            .parent
            .map(|p| computed[p.0].clone())
            .unwrap_or_default();
        computed[id.0] = rules.compute(&doc, id, &parent, root_font);
        if doc.tag(id) == "html" {
            root_font = computed[id.0].font_size;
        }
        stack.extend(doc.nodes[id.0].children.iter().rev().copied());
    }
    (doc, computed)
}
fn by_id(doc: &Document, name: &str) -> NodeId {
    doc.nodes
        .iter()
        .enumerate()
        .find_map(|(i, _)| (doc.attr(NodeId(i), "id") == Some(name)).then_some(NodeId(i)))
        .unwrap()
}

#[test]
fn lexical_boundaries_and_at_rule_recovery() {
    let rules = css::parse(
        r#"@charset "UTF-8"; /* } ; */ [data-x="a,b}"] {font-family:"/* not a comment */;x\";z";color:rgb(1,2,3)} p {color:blue}"#,
        800.,
    );
    assert_eq!(rules.len(), 2);
    assert_eq!(rules[0].selector, r#"[data-x="a,b}"]"#);
    assert_eq!(rules[0].declarations.len(), 2);
    assert_eq!(
        rules[0].declarations[0].value,
        r#""/* not a comment */;x\";z""#
    );
    let d = css::declarations(
        r#"color: red ! /* gap */ IMPORTANT; --a: "!important"; background:url("a;:!important") blue; padding:1px/*;*/ 2px"#,
    );
    assert_eq!(d.len(), 4);
    assert!(d[0].important);
    assert!(!d[1].important);
    assert!(!d[2].important);
}

#[test]
fn css_bad_string_drops_one_declaration_and_preserves_following_rules() {
    for newline in ["\n", "\r", "\r\n", "\x0c"] {
        let body = format!(
            "width:29px; --broken:\"bad{newline}/* ; width:13px; }} */;\
             --nested:fn('bad{newline};still-nested); height:19px; width:41px"
        );
        let declarations = css::declarations(&body);
        let pairs: Vec<_> = declarations
            .iter()
            .map(|d| (d.name.as_str(), d.value.as_str()))
            .collect();
        assert_eq!(
            pairs,
            [("width", "29px"), ("height", "19px"), ("width", "41px")],
            "{newline:?}"
        );

        let sheet = format!("#broken {{{body}}} #later {{width:43px}}");
        let rules = css::parse(&sheet, 800.);
        assert_eq!(rules.len(), 2, "{newline:?}");
        assert_eq!(rules[0].declarations.len(), 3);
        assert_eq!(rules[1].selector, "#later");
        let (doc, computed) = styles("<div id=broken></div><div id=later></div>", &sheet);
        assert_eq!(computed[by_id(&doc, "broken").0].width, Length::Px(41.));
        assert_eq!(computed[by_id(&doc, "later").0].width, Length::Px(43.));
    }
}

#[test]
fn css_bad_string_recovery_does_not_treat_escaped_newlines_as_bad() {
    for newline in ["\n", "\r", "\r\n", "\x0c"] {
        for escape in ["\\", "\\61"] {
            let value = format!("\"a{escape}{newline}/* literal */;z\"");
            let source = format!("--keep:{value}; width:31px");
            let declarations = css::declarations(&source);
            assert_eq!(declarations.len(), 2, "{source:?}");
            assert_eq!(declarations[0].value, value);
            assert_eq!(declarations[1].name, "width");
        }
    }
}

#[test]
fn css_block_recovery_preserves_declarations_and_the_next_rule() {
    for (value, width) in [
        ("fn(})", 41.),
        ("[}]", 47.),
        ("fn(a;b, [{}])", 31.),
        (r"fn(\})", 37.),
        (r#"fn("}")"#, 39.),
    ] {
        let sheet = format!(
            "#first {{ width:29px; color:green; color:{value}; width:{width}px; }} \
             #later {{ width:53px; }}"
        );
        let (doc, computed) = styles("<div id=first></div><div id=later></div>", &sheet);
        let first = &computed[by_id(&doc, "first").0];
        assert_eq!(first.width, Length::Px(width), "{value}");
        assert_eq!(first.color, Color::rgb(0, 128, 0), "{value}");
        assert_eq!(computed[by_id(&doc, "later").0].width, Length::Px(53.));
    }
}

#[test]
fn pseudo_elements_preserve_element_branches_in_selector_lists() {
    let (doc, computed) = styles(
        "<section id='section'><div id='item'></div></section>",
        ":root {box-sizing:border-box} *,:after,::before {box-sizing:inherit}
         #item::before {width:75px} #item,::unknown-pseudo {width:99px}",
    );
    let item = by_id(&doc, "item");
    assert_eq!(computed[item.0].box_sizing, css::BoxSizing::BorderBox);
    assert_eq!(computed[item.0].width, Length::Auto);
    assert!(css::matches_selector(&doc, item, "#item, ::after"));
    assert!(!css::matches_selector(&doc, item, "#item::before"));
    assert!(!css::matches_selector(&doc, item, "#item:not(::after)"));
}

#[test]
fn css_block_recovery_does_not_open_blocks_in_unquoted_urls() {
    // Only test rule discovery; declaration-value parsing is a separate step.
    for value in [
        r"URL(asset[\).svg)",
        "url(asset{.svg)",
        r#"url("asset[.svg")"#,
        r"u\72l(asset[.svg)",
        r"\url(asset{.svg)",
        r"\55\52\00004C(asset[.svg)",
        "u\\72\r\nl(asset[.svg)",
        "u\\72\x0cl(asset{.svg)",
        r"u\72l(bad([\).svg)",
        r#"u\72l( "asset) } [.svg")"#,
    ] {
        let sheet = format!("#first {{ --asset:{value} }} #later {{ width:53px; }}");
        let rules = css::parse(&sheet, 800.);
        assert_eq!(rules.len(), 2, "{value}");
        assert_eq!(rules[1].selector, "#later", "{value}");
    }
}

#[test]
fn css_block_recovery_only_treats_a_url_token_as_opaque() {
    for head in [
        r"x-\75 rl",
        r"x\61 url",
        r"1.5\75 rl",
        r"#\75 rl",
        r"@\75 rl",
        r"u/**/\72l",
        r"u\72l/**/",
        r"u\72l ",
    ] {
        // The second `}` is outside the inner function, but still nested.
        let sheet = format!("#first {{ --asset:{head}(fn(}}) }}) }} #later {{ width:53px; }}");
        let rules = css::parse(&sheet, 800.);
        assert_eq!(rules.len(), 2, "{head}");
        assert_eq!(rules[1].selector, "#later", "{head}");
    }
}

#[test]
fn longhand_cascade_is_independent_of_shorthand_priority() {
    let (d, s) = styles(
        "<div id='a' class='c' style='padding-left:7px; color: blue'></div>",
        "#a{margin:10px;padding:20px}.c{margin-left:50px!important;padding-left:3px;color:red!important} #a{margin-top:8px;margin:2px}",
    );
    let a = &s[by_id(&d, "a").0];
    assert_eq!(
        a.margin,
        [
            Length::Px(2.),
            Length::Px(2.),
            Length::Px(2.),
            Length::Px(50.)
        ]
    );
    assert_eq!(a.padding[3], Length::Px(7.));
    assert_eq!(a.color, Color::rgb(255, 0, 0));
    let (d, s) = styles(
        "<p id='a' style='margin:4px!important; margin-left:1px; color:blue!important'></p>",
        "#a{margin-left:90px!important;color:red!important}",
    );
    assert_eq!(s[by_id(&d, "a").0].margin[3], Length::Px(4.));
    assert_eq!(s[by_id(&d, "a").0].color, Color::rgb(0, 0, 255));
}
#[test]
fn invalid_values_do_not_win_and_initial_is_not_ua_default() {
    let (d, s) = styles(
        "<div style='color:purple;font-size:20px;line-height:30px'><h1 id='a' style='display:initial;color:unset;font-size:10px'></h1><p id='b' style='color:red;color:nonsense;width:10px;width:7;padding:3px;padding:-2px'></p></div>",
        "",
    );
    let a = &s[by_id(&d, "a").0];
    assert_eq!(a.display, Display::Inline);
    assert_eq!(a.color, Color::rgb(128, 0, 128));
    assert_eq!(a.line_height_px(), 30.);
    let b = &s[by_id(&d, "b").0];
    assert_eq!(b.color, Color::rgb(255, 0, 0));
    assert_eq!(b.width, Length::Px(10.));
    assert_eq!(b.padding, [Length::Px(3.); 4]);
}
#[test]
fn root_font_units_variables_and_shorthand_substitution() {
    let (d, s) = styles(
        "<html><body><div id='a'><span id='b'></span><span id='c' style='--accent:blue'></span></div></body></html>",
        "html{font-size:20px;--accent:red;--derived:var(--accent)} #a{font-size:2rem;padding:var(--space, 2px 3px);--loop:var(--loop);color:var(--loop, green);width:10vw} #b{color:var(--missing, var(--accent));margin:1rem} #c{color:var(--derived)}",
    );
    let a = &s[by_id(&d, "a").0];
    assert_eq!(a.font_size, 40.);
    assert_eq!(
        a.padding,
        [
            Length::Px(2.),
            Length::Px(3.),
            Length::Px(2.),
            Length::Px(3.)
        ]
    );
    assert_eq!(a.width, Length::Px(80.));
    assert_eq!(a.color, Color::rgb(0, 128, 0));
    let b = &s[by_id(&d, "b").0];
    assert_eq!(b.color, Color::rgb(255, 0, 0));
    assert_eq!(b.margin, [Length::Px(20.); 4]);
    assert_eq!(
        s[by_id(&d, "c").0].color,
        Color::rgb(255, 0, 0),
        "custom properties inherit their computed substitution"
    );
}
#[test]
fn selectors_backtrack_ancestors_and_honor_specificity() {
    let (d, s) = styles(
        "<div class='a'><section class='b'><div><section class='b'><p id='target' class='x' data-note='Hello world'></p><p id='second'></p></section></div></section></div>",
        ".a>.b .x{color:green}:where(#target){color:red}:is(p,.x){padding:2px} p:not(.missing){box-sizing:border-box}",
    );
    let p = by_id(&d, "target");
    assert!(css::matches_selector(&d, p, ".a > .b .x"));
    assert!(css::matches_selector(
        &d,
        p,
        "[data-note^='hello' i][data-note~='world']"
    ));
    assert!(!css::matches_selector(&d, p, "[data-note*='']"));
    assert_eq!(s[p.0].color, Color::rgb(0, 128, 0));
    assert_eq!(s[p.0].box_sizing, BoxSizing::BorderBox);
    let second = by_id(&d, "second");
    assert!(css::matches_selector(
        &d,
        second,
        "p + p:last-child:nth-child(2n)"
    ));
    assert!(css::matches_selector(&d, second, ".x ~ p"));
    assert!(!css::matches_selector(&d, p, "p:not(:made-up)"));
    assert!(css::matches_selector(&d, p, "#tar\\67 et"));
}
#[test]
fn media_lists_are_or_conditions_and_unknown_features_do_not_leak() {
    let sheet = "@media print, screen and (min-width:600px){p{color:red}} @media (made-up:yes){p{color:blue}} @media screen and (max-width:300px){p{padding:1px}}";
    assert_eq!(css::parse(sheet, 800.).len(), 1);
    assert_eq!(css::parse(sheet, 200.).len(), 1);
}
#[test]
fn indexed_candidates_skip_unrelated_rules_and_keep_source_order() {
    let d = html::parse("<p id='x' class='c7 shared'></p>");
    let p = by_id(&d, "x");
    let mut source = String::new();
    for i in 0..4000 {
        source.push_str(&format!(".c{i}{{color:red}}"));
    }
    source.push_str("p{color:green}.shared{color:blue}#x{color:purple}");
    let sheet = Stylesheet::new(css::parse(&source, 800.), 800.);
    assert_eq!(sheet.candidate_rule_count(&d, p), 4);
    assert_eq!(
        sheet.compute(&d, p, &Style::default(), 16.).color,
        Color::rgb(128, 0, 128)
    );
}
#[test]
fn border_shorthand_resets_omitted_components_and_expands_sides() {
    let (d, s) = styles(
        "<div id='a'></div>",
        "#a{border:4px solid red;border-left-width:2px;border-top:solid;border-right-color:blue}",
    );
    let a = &s[by_id(&d, "a").0];
    assert_eq!(a.borders(), [3., 4., 4., 2.]);
    assert_eq!(
        a.border_colors,
        [
            None,
            Some(Color::rgb(0, 0, 255)),
            Some(Color::rgb(255, 0, 0)),
            Some(Color::rgb(255, 0, 0))
        ]
    );
    assert_eq!(
        css::color("rgb(100% 0% 0% / 50%)"),
        Some(Color(255, 0, 0, 128))
    );
    assert_eq!(css::color("hsl(120 100% 25%)"), Some(Color(0, 128, 0, 255)));
}

#[test]
fn cssom_validation_accepts_only_one_valid_supported_value() {
    assert!(css::supports_declaration("color", "red"));
    assert!(!css::supports_declaration("color", "garbage"));
    assert!(!css::supports_declaration("color", "red;display:none"));
    assert!(!css::supports_declaration(
        "background-color",
        "url(x) blue"
    ));
    assert!(css::supports_declaration("margin", "var(--space, 2px 3px)"));
    assert!(css::supports_declaration("color", "inherit"));
    assert!(css::supports_declaration("--my-color", "red"));
    assert!(css::supports_declaration("flex", "1 1 0"));
    assert!(!css::supports_declaration("color", "var(--space"));
    assert!(!css::supports_declaration("color", "var(red)"));
    assert!(!css::supports_declaration("margin", "3px -1px !important"));
}

#[test]
fn custom_property_cycles_include_unused_fallbacks() {
    let (d, s) = styles(
        "<p id='a'></p>",
        "#a{--ok:red;--bad:var(--ok,var(--bad));color:var(--bad,green)}",
    );
    assert_eq!(s[by_id(&d, "a").0].color, Color::rgb(0, 128, 0));
}
#[test]
fn selector_backtracking_and_selector_list_expansion_are_bounded() {
    let source = format!(
        "{}<p id='x'></p>{}",
        "<div class='a'>".repeat(64),
        "</div>".repeat(64)
    );
    let doc = html::parse(&source);
    let selector = format!(".missing {}#x", ".a ".repeat(20));
    assert!(!css::matches_selector(&doc, by_id(&doc, "x"), &selector));
    let selectors = (0..1000)
        .map(|i| format!(".c{i}"))
        .collect::<Vec<_>>()
        .join(",");
    let rules = css::parse(
        &format!("{selectors}{{--large:{}}}", "x".repeat(65536)),
        800.,
    );
    assert!(rules.len() < 300);
    assert!(!rules.is_empty());
}

#[test]
fn document_styles_keep_host_selectors_in_shared_theme_lists() {
    let (doc, computed) = styles(
        "<html><body><p id='sample'>Result</p></body></html>",
        ":root, :host { --ink: #203040 } :root:not(.dark), :host(:not(.dark)) { --pad: 18px }
         :host { --ink: red } :host(html) { --pad: 0 }
         p { color: var(--ink); width: var(--pad) }",
    );
    let sample = &computed[by_id(&doc, "sample").0];
    assert_eq!(sample.color, Color::rgb(32, 48, 64));
    assert_eq!(sample.width, Length::Px(18.));
    for invalid in [
        ":host()",
        ":host(html,.banner)",
        ":host(.a .b)",
        ":host(.a > .b)",
    ] {
        assert!(css::parse(&format!(":root, {invalid} {{ color: red }}"), 800.).is_empty());
    }
}

#[test]
fn responsive_width_ranges_handle_minified_reversed_and_chained_boundaries() {
    let matches = |query: &str, width| {
        !css::parse(
            &format!("@media {query} {{ nav {{ display: flex }} }}"),
            width,
        )
        .is_empty()
    };
    for query in [
        "(width>=816px)",
        "(51em<=width)",
        "(width = 816px)",
        "(800px < width <= 816px)",
        "(816px >= width > 800px)",
        "(width>=800px) and (width<=816px)",
    ] {
        assert!(matches(query, 816.), "{query}");
    }
    for query in [
        "(width>816px)",
        "(816px < width)",
        "(width < = 820px)",
        "(width<=100%)",
        "(min-width:30%)",
        "(80px<width=816px)",
        "(80px<width>=700px)",
        "(width<820px<900px)",
        "(width < width)",
        "(width < 900px > 600px)",
        "(height <= 816px)",
    ] {
        assert!(!matches(query, 816.), "{query}");
    }
    assert!(matches("(width<=816px)", 815.99));
    assert!(!matches("(width<=816px)", 816.01));
}

#[test]
fn large_theme_computes_all_variables_and_inherits_computed_values() {
    let mut sheet = String::from(":root, :host {");
    for i in 0..900 {
        sheet.push_str(&format!("--theme-{i}:{i}px;"));
    }
    sheet.push_str("--result-width:var(--theme-899);}");
    sheet.push_str("main {--theme-899:1px;--local:var(--result-width)} p {width:var(--local)}");
    let (doc, computed) = styles(
        "<html><body><main><p id='sample'></p></main></body></html>",
        &sheet,
    );
    let sample = &computed[by_id(&doc, "sample").0];
    assert_eq!(sample.width, Length::Px(899.));
    for i in 0..899 {
        assert_eq!(
            sample.custom_property(&format!("--theme-{i}")),
            Some(format!("{i}px").as_str())
        );
    }
    assert_eq!(sample.custom_property("--theme-899"), Some("1px"));
}

#[test]
fn over_limit_theme_admits_new_names_in_stable_declaration_order() {
    let mut sheet = String::from(":root{");
    for i in 0..2050 {
        sheet.push_str(&format!("--t{i}:{i}px;"));
    }
    sheet.push_str("}p{--t0:7px; width:var(--t0)}");
    let (doc, computed) = styles("<html><body><p id='sample'></p></body></html>", &sheet);
    let sample = &computed[by_id(&doc, "sample").0];
    assert_eq!(sample.width, Length::Px(7.));
    assert_eq!(sample.custom_property("--t2047"), Some("2047px"));
    assert_eq!(sample.custom_property("--t2048"), None);
}

#[test]
fn list_marker_shorthand_respects_wide_keywords_and_invalid_values() {
    use css::MarkerType;
    let (doc, computed) = styles(
        "<ol><li id='first'></li><li id='second'></li><li id='third'></li><li id='fourth'></li></ol>",
        "ol { list-style: none } #first { list-style-type: inherit } #second { list-style: initial }
         #third { list-style: none outside square; list-style-type: 14px }
         #fourth { list-style: outside; list-style: circle decimal }"
    );
    for (id, marker) in [
        ("first", MarkerType::None),
        ("second", MarkerType::Disc),
        ("third", MarkerType::Square),
        ("fourth", MarkerType::Disc),
    ] {
        assert_eq!(computed[by_id(&doc, id).0].marker, marker, "{id}");
    }
}
