//! Pinned upstream reftests. The fixture XML and reference XML are kept byte
//! for byte; our HTML foreign-content parser and CSS cascade consume both.
use rust_browser_engine::{
    css::{self, Style, Stylesheet},
    dom::{Namespace, NodeKind},
    svg::{SvgRect, SvgScene, parse_document},
};
use std::collections::BTreeMap;

const CASES: [(&str, &str, &str); 7] = [
    (
        "circle-01",
        include_str!("../src/svg/fixtures/circle-01.svg"),
        include_str!("../src/svg/fixtures/circle-01-ref.svg"),
    ),
    (
        "ellipse-01",
        include_str!("../src/svg/fixtures/ellipse-01.svg"),
        include_str!("../src/svg/fixtures/ellipse-01-ref.svg"),
    ),
    (
        "ellipse-02",
        include_str!("../src/svg/fixtures/ellipse-02.svg"),
        include_str!("../src/svg/fixtures/ellipse-02-ref.svg"),
    ),
    (
        "ellipse-03",
        include_str!("../src/svg/fixtures/ellipse-03.svg"),
        include_str!("../src/svg/fixtures/ellipse-03-ref.svg"),
    ),
    (
        "ellipse-04",
        include_str!("../src/svg/fixtures/ellipse-04.svg"),
        include_str!("../src/svg/fixtures/ellipse-04-ref.svg"),
    ),
    (
        "ellipse-07",
        include_str!("../src/svg/fixtures/ellipse-07.svg"),
        include_str!("../src/svg/fixtures/ellipse-07-ref.svg"),
    ),
    (
        "line-dasharray",
        include_str!("../src/svg/fixtures/line-dasharray.svg"),
        include_str!("../src/svg/fixtures/line-dasharray-ref.svg"),
    ),
];

fn render(source: &str) -> Result<(u32, u32, Vec<u32>), String> {
    let doc = parse_document(source).map_err(|error| error.to_string())?;
    if doc.truncated {
        return Err("HTML parse truncated".into());
    }
    let root = doc
        .elements("svg")
        .find(|&id| doc.namespace(id) == Namespace::Svg)
        .ok_or("missing SVG root")?;
    let (width, height) = SvgScene::intrinsic_size(&doc, root);
    let (width, height) = (width.ceil() as u32, height.ceil() as u32);
    if width == 0 || height == 0 || width > 1024 || height > 1024 {
        return Err("test surface exceeds fixture harness bounds".into());
    }
    let rules = doc
        .elements("style")
        .flat_map(|id| css::parse(&doc.text(id), width as f32))
        .collect();
    let stylesheet = Stylesheet::new(rules, width as f32);
    let mut computed = vec![Style::default(); doc.nodes.len()];
    let mut overrides = BTreeMap::new();
    for id in doc.connected_nodes() {
        if !matches!(doc.nodes[id.0].kind, NodeKind::Element { .. }) {
            continue;
        }
        let parent = doc.nodes[id.0]
            .parent
            .map(|id| computed[id.0].clone())
            .unwrap_or_default();
        let style = stylesheet.compute(&doc, id, &parent, 16.);
        if doc.namespace(id) == Namespace::Svg {
            overrides.insert(id, style.svg_style());
        }
        computed[id.0] = style;
    }
    let scene = SvgScene::build(&doc, root, width as f32, height as f32, &overrides);
    if scene.diagnostics.truncated
        || scene.diagnostics.invalid_geometry
        || scene.diagnostics.unsupported
    {
        return Err(format!("scene diagnostics: {:?}", scene.diagnostics));
    }
    let bounds = SvgRect {
        x: 0.,
        y: 0.,
        width: width as f32,
        height: height as f32,
    };
    let mut pixels = vec![0xffffff; width as usize * height as usize];
    let report = scene.paint(&mut pixels, width, height, bounds, bounds);
    if report.truncated || report.invalid_surface {
        return Err(format!("render diagnostics: {report:?}"));
    }
    Ok((width, height, pixels))
}

#[test]
fn pinned_wpt_shape_reftests_match_every_reference_without_exclusions() {
    let mut failed = Vec::new();
    for (name, source, reference) in CASES {
        match (render(source), render(reference)) {
            (Ok((w, h, actual)), Ok((rw, rh, expected))) if (w, h) == (rw, rh) => {
                let mismatches = actual.iter().zip(&expected).filter(|(a, b)| a != b).count();
                if mismatches != 0 {
                    failed.push(format!(
                        "{name}: {mismatches}/{} pixels differ",
                        actual.len()
                    ));
                }
            }
            (actual, reference) => failed.push(format!(
                "{name}: actual {:?}; reference {:?}",
                actual.as_ref().map(|(w, h, _)| (w, h)),
                reference.as_ref().map(|(w, h, _)| (w, h))
            )),
        }
    }
    eprintln!(
        "Pinned WPT SVG shapes: {} passed / {} failed / {} total; 0 skipped",
        CASES.len() - failed.len(),
        failed.len(),
        CASES.len()
    );
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}
