use rust_browser_engine::{
    LoadedPage,
    css::{self, Color, Display, GridAutoFlow, GridBreadth, GridLine, Length, Style, Stylesheet},
};
fn style(page: &LoadedPage, rules: &str, name: &str) -> Style {
    let id = page
        .document
        .connected_nodes()
        .find(|&id| page.document.attr(id, "id") == Some(name))
        .unwrap();
    Stylesheet::new(css::parse(rules, 800.), 800.).compute(
        &page.document,
        id,
        &Style::default(),
        16.,
    )
}
#[test]
fn comment_boundaries_preserve_compounds_without_merging_identifiers() {
    let page = LoadedPage::from_html(
        "about:css",
        "<!doctype html><div class='x' id='outer'><span id='inner' class='x' data-type='word'></span></div>",
    );
    let rules = "div/**/.x{background:red} div/**/ .x{color:blue} div/**/span{font-size:999px} [data-type/**/=/**/word]{font-weight:bold}";
    let outer = style(&page, rules, "outer");
    let inner = style(&page, rules, "inner");
    assert_eq!(outer.background, Some(Color::rgb(255, 0, 0)));
    assert_eq!(outer.color, Color::BLACK);
    assert_eq!(inner.color, Color::rgb(0, 0, 255));
    assert!(inner.bold);
    assert_eq!(inner.font_size, 16.);
    let id = page
        .document
        .connected_nodes()
        .find(|&id| page.document.attr(id, "id") == Some("outer"))
        .unwrap();
    assert!(css::matches_selector(&page.document, id, "div/**/.x"));
    assert!(!css::matches_selector(&page.document, id, "di/**/v.x"));
    assert!(!css::matches_selector(&page.document, id, "div\0.x"));
    assert!(!css::matches_selector(&page.document, id, "#/**/outer"));
    let inner_id = page
        .document
        .connected_nodes()
        .find(|&id| page.document.attr(id, "id") == Some("inner"))
        .unwrap();
    assert!(css::matches_selector(
        &page.document,
        inner_id,
        "[data-type/**/=/**/word/**/i]"
    ));
    assert!(
        !css::declarations("color:rgb(1/**/ 2/**/ 3)")[0]
            .value
            .contains('\0')
    );
}
#[test]
fn declaration_comments_and_variables_cannot_forge_dimension_or_keyword_tokens() {
    assert!(css::supports_declaration("width", "10px/**/"));
    assert!(!css::supports_declaration("width", "10/**/px"));
    assert!(!css::supports_declaration("color", "bl/**/ue"));
    assert!(css::supports_declaration("color", "rgb(1/**/ 2/**/ 3)"));
    let page = LoadedPage::from_html("about:css", "<div id='a'></div>");
    let result = style(
        &page,
        "#a{color:green;col/**/or:red;--n:10;width:var(--n)px}",
        "a",
    );
    assert_eq!(result.width, Length::Auto);
    assert_eq!(result.color, Color::rgb(0, 128, 0));
    let result = style(&page, "#a{--size:10px;width:var(--size)}", "a");
    assert_eq!(result.width, Length::Px(10.));
}
#[test]
fn grid_shorthands_tracks_and_invalid_cascade() {
    let page = LoadedPage::from_html("about:css", "<div id='a'></div>");
    let result = style(
        &page,
        "#a{display:grid;grid-template:repeat(2,30px)/100px minmax(0,2fr);grid-auto-flow:dense column;grid-area:2/3/span 2/-1;grid-template-columns:-1fr}",
        "a",
    );
    assert_eq!(result.display, Display::Grid);
    assert_eq!(result.grid_template_rows.len(), 2);
    assert_eq!(result.grid_template_columns.len(), 2);
    assert_eq!(result.grid_template_columns[1].max, GridBreadth::Flex(2.));
    assert_eq!(result.grid_row, [GridLine::Line(2), GridLine::Span(2)]);
    assert_eq!(result.grid_column, [GridLine::Line(3), GridLine::Line(-1)]);
    assert_eq!(result.grid_auto_flow, GridAutoFlow::Column);
    assert!(result.grid_dense);
    assert!(!css::supports_declaration(
        "grid-template-columns",
        "minmax(1fr,2fr)"
    ));
    assert!(!css::supports_declaration(
        "grid-template-columns",
        "minmax(0,-2fr)"
    ));
    assert!(!css::supports_declaration(
        "grid-template-columns",
        "repeat(1000000,1px)"
    ));
    assert!(!css::supports_declaration("grid-row", "span 0"));
    assert!(css::supports_declaration(
        "grid-template-columns",
        "1fr/**/2fr"
    ));
}

#[test]
fn grid_template_areas_and_placement_shorthands_cascade_as_longhands() {
    let page = LoadedPage::from_html("about:css", "<div id='board'><div id='card'></div></div>");
    let rules = r#"
        #board {
            --first: [Alpha-start] 2rem [Middle];
            grid-template: [top] 'Header Header' 18px [divider] [below]
                           'Nav Main' [bottom] / var(--first) 1fr [Alpha-end];
            grid-template-areas: "Header Header" "Header Nav";
        }
        #card { grid-area: Main / -2 cell; grid-column-end: SPAN 2 \43 ell; }
    "#;
    let board = style(&page, rules, "board");
    assert_eq!(board.grid_template_areas.dimensions(), [2, 2]);
    assert_eq!(board.grid_template_columns.len(), 2);
    assert_eq!(
        board.grid_template_columns[0].max,
        GridBreadth::Length(Length::Px(32.))
    );
    assert_eq!(
        board.grid_template_rows[0].max,
        GridBreadth::Length(Length::Px(18.))
    );
    assert_eq!(board.grid_template_rows[1].max, GridBreadth::Auto);

    let card = style(&page, rules, "card");
    let main = GridLine::Named {
        name: "Main".into(),
        occurrence: None,
    };
    assert_eq!(card.grid_row, [main.clone(), main]);
    assert_eq!(
        card.grid_column,
        [
            GridLine::Named {
                name: "cell".into(),
                occurrence: Some(-2)
            },
            GridLine::NamedSpan {
                name: "Cell".into(),
                count: 2
            },
        ]
    );
    for rules in [
        r#"#board{grid-template-areas:'A A';grid-template:none}"#,
        r#"#board{grid-template-areas:'A A';grid-template:18px / 1fr 1fr}"#,
    ] {
        assert_eq!(
            style(&page, rules, "board")
                .grid_template_areas
                .dimensions(),
            [0, 0]
        );
    }
}

#[test]
fn grid_validates_track_lists_area_matrices_and_named_line_grammar() {
    for (property, source) in [
        (
            "grid-template-columns",
            r"[frame-start]10px[O] repeat(2,[\31st]10px[O])[frame-end]",
        ),
        (
            "grid-template-columns",
            "repeat(2,minmax(auto,20px)) REPEAT(AUTO-FIT, [tile] minmax(1em, 1fr))",
        ),
        (
            "grid-template-rows",
            "repeat(auto-fill, [item] 12% minmax(0,2fr) [edge])",
        ),
        ("grid-template-columns", "[] 1fr []"),
        ("grid-template-areas", r#""Area...X" "Area . Y""#),
        ("grid-template-areas", r#""1st 1st" ".. \65 nd""#),
        ("grid-area", r"\31st / -2 Trail / span Tail 3"),
        ("grid-column", "2 Named / span Named"),
    ] {
        assert!(
            css::supports_declaration(property, source),
            "{property}: {source}"
        );
    }
    for (property, source) in [
        ("grid-template-columns", "[span] 10px"),
        ("grid-template-columns", "[auto] 10px"),
        ("grid-template-columns", "[1st] 10px"),
        ("grid-template-columns", "[x][y] 10px"),
        ("grid-template-columns", "repeat(auto-fit,1fr)"),
        (
            "grid-template-columns",
            "repeat(auto-fill,minmax(auto,max-content))",
        ),
        (
            "grid-template-columns",
            "2fr repeat(auto-fit,minmax(12px,1fr))",
        ),
        (
            "grid-template-columns",
            "repeat(2,auto) repeat(auto-fit,20px)",
        ),
        (
            "grid-template-columns",
            "repeat(auto-fill,20px) repeat(auto-fit,20px)",
        ),
        ("grid-template-columns", "repeat(auto-fill,repeat(2,10px))"),
        (
            "grid-template-columns",
            "repeat(128,10px) repeat(auto-fit,20px)",
        ),
        ("grid-template-columns", "repeat(auto-fill,[])"),
        ("grid-auto-columns", "repeat(auto-fit,20px)"),
        ("grid-auto-columns", "[name] 20px"),
        ("grid-template-areas", r#""A A" "A B""#),
        ("grid-template-areas", r#""A A" "B""#),
        ("grid-template-areas", r#""A x/y""#),
        ("grid-template-areas", r#"".. .." " ""#),
        ("grid-template", r#""A A" / repeat(2,1fr)"#),
        ("grid-template", r#""A" repeat(2,10px)"#),
        ("grid-column", "0 thing"),
        ("grid-column", "span -1 thing"),
        ("grid-column", "span span"),
        ("grid-column", "2 thing else"),
        ("grid-column", "auto /"),
    ] {
        assert!(
            !css::supports_declaration(property, source),
            "{property}: {source}"
        );
    }
}
