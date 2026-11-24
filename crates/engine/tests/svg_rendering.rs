use rust_browser_engine::{
    css::{self, Style, Stylesheet},
    dom::{Document, Namespace, NodeId, NodeKind},
    html,
    svg::{SvgRect, SvgScene, SvgStyle},
};
use std::collections::BTreeMap;

fn build(source: &str, width: u32, height: u32) -> (Document, SvgScene) {
    let doc = html::parse(source);
    let root = doc.elements("svg").next().unwrap();
    let scene = SvgScene::build(&doc, root, width as f32, height as f32, &BTreeMap::new());
    (doc, scene)
}
fn build_cascaded(source: &str, width: u32, height: u32) -> (Document, SvgScene) {
    let doc = html::parse(source);
    let root = doc.elements("svg").next().unwrap();
    let stylesheet = Stylesheet::new(
        doc.elements("style")
            .flat_map(|id| css::parse(&doc.text(id), width as f32))
            .collect(),
        width as f32,
    );
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
            let mut svg = style.svg_style();
            if id == root {
                svg.color = Some(style.color);
                svg.inherit.remove("color");
            }
            overrides.insert(id, svg);
        }
        computed[id.0] = style;
    }
    let scene = SvgScene::build(&doc, root, width as f32, height as f32, &overrides);
    (doc, scene)
}
fn image(scene: &SvgScene, width: u32, height: u32) -> Vec<u32> {
    let mut pixels = vec![0xffffff; width as usize * height as usize];
    let report = scene.paint(
        &mut pixels,
        width,
        height,
        SvgRect {
            x: 0.,
            y: 0.,
            width: width as f32,
            height: height as f32,
        },
        SvgRect {
            x: 0.,
            y: 0.,
            width: width as f32,
            height: height as f32,
        },
    );
    assert!(!report.invalid_surface && !report.truncated, "{report:?}");
    pixels
}
fn pixel(p: &[u32], width: usize, x: usize, y: usize) -> u32 {
    p[y * width + x]
}
fn node(doc: &Document, name: &str) -> NodeId {
    doc.connected_nodes()
        .find(|&id| doc.attr(id, "id") == Some(name))
        .unwrap()
}
fn near(actual: u32, expected: u32) {
    for shift in [0, 8, 16] {
        assert!(
            (((actual >> shift) & 255) as i32 - ((expected >> shift) & 255) as i32).abs() <= 2,
            "{actual:06x} != {expected:06x}"
        );
    }
}

#[test]
fn object_bounds_ignore_stroke_clips_and_paint_but_keep_zero_extent_lines() {
    let (doc, scene) = build(
        "<svg><defs><clipPath id=clip><rect width=5 height=5 /></clipPath><rect id=definition width=90 height=90 /></defs><g id=group transform='translate(10 5)'><circle id=circle cx=50 cy=50 r=45 fill=none stroke=red stroke-width=4 clip-path='url(#clip)' /><line id=line x1=120 y1=20 x2=140 y2=20 fill=none /><rect id=hidden x=150 width=10 height=10 visibility=hidden /><rect id=none x=200 width=10 height=10 display=none /></g></svg>",
        240,
        110,
    );
    let circle = node(&doc, "circle");
    assert_eq!(
        scene.geometry_bounds[&circle],
        SvgRect {
            x: 15.,
            y: 10.,
            width: 90.,
            height: 90.,
        }
    );
    assert!(scene.node_bounds[&circle].width < 90.);
    assert_eq!(scene.geometry_bounds[&node(&doc, "line")].height, 0.);
    assert!(scene.geometry_bounds.contains_key(&node(&doc, "hidden")));
    assert!(!scene.geometry_bounds.contains_key(&node(&doc, "none")));
    assert!(
        !scene
            .geometry_bounds
            .contains_key(&node(&doc, "definition"))
    );
    assert_eq!(scene.geometry_bounds[&node(&doc, "group")].width, 155.);
    assert_eq!(scene.hit_test(60., 55.), None);
}

#[test]
fn basic_shapes_generate_real_fill_and_stroke_pixels() {
    let (_, s) = build(
        "<svg><rect x=2 y=2 width=18 height=16 fill=red /><circle cx=40 cy=10 r=8 fill=green /><ellipse cx=70 cy=10 rx=12 ry=7 fill=blue /><line x1=5 y1=40 x2=25 y2=40 stroke=red stroke-width=6 /><polyline points='35,45 45,30 55,45' fill=none stroke=green stroke-width=3 /><polygon points='65,45 75,30 85,45' fill=blue /></svg>",
        100,
        60,
    );
    assert!(
        !s.diagnostics.truncated && !s.diagnostics.invalid_geometry,
        "{:?}",
        s.diagnostics
    );
    let p = image(&s, 100, 60);
    assert_eq!(pixel(&p, 100, 10, 10), 0xff0000);
    assert_eq!(pixel(&p, 100, 40, 10), 0x008000);
    assert_eq!(pixel(&p, 100, 70, 10), 0x0000ff);
    assert_eq!(pixel(&p, 100, 15, 40), 0xff0000);
    assert_eq!(pixel(&p, 100, 75, 40), 0x0000ff);
    assert_eq!(pixel(&p, 100, 1, 1), 0xffffff);
}

#[test]
fn viewbox_default_meet_none_slice_and_nonzero_origin() {
    let (_, s) = build(
        "<svg viewBox='10 20 100 100'><rect x=10 y=20 width=100 height=100 fill=blue /></svg>",
        200,
        100,
    );
    let p = image(&s, 200, 100);
    assert_eq!(pixel(&p, 200, 25, 50), 0xffffff);
    assert_eq!(pixel(&p, 200, 75, 50), 0x0000ff);
    assert_eq!(pixel(&p, 200, 175, 50), 0xffffff);
    let (_, s) = build(
        "<svg viewBox='10 20 100 100' preserveAspectRatio=none><rect x=10 y=20 width=100 height=100 fill=blue /></svg>",
        200,
        100,
    );
    let p = image(&s, 200, 100);
    assert!(p.iter().all(|&c| c == 0x0000ff));
    let (_, s) = build(
        "<svg viewBox='0 0 100 100' preserveAspectRatio='xMaxYMax slice'><rect width=100 height=50 fill=red /><rect y=50 width=100 height=50 fill=blue /></svg>",
        200,
        100,
    );
    let p = image(&s, 200, 100);
    assert_eq!(pixel(&p, 200, 100, 10), 0x0000ff);
}

#[test]
fn group_transforms_and_current_color_inherit_as_paint_values() {
    let (doc, s) = build(
        "<svg color=blue><g transform='translate(10 5) scale(2)' fill=currentColor><rect id=a width=10 height=10 /><rect id=b x=15 width=10 height=10 color=red /></g></svg>",
        80,
        40,
    );
    let p = image(&s, 80, 40);
    assert_eq!(pixel(&p, 80, 15, 10), 0x0000ff);
    assert_eq!(pixel(&p, 80, 45, 10), 0xff0000);
    assert_eq!(pixel(&p, 80, 5, 10), 0xffffff);
    assert_eq!(s.hit_test(15., 10.), Some(node(&doc, "a")));
    assert_eq!(s.hit_test(45., 10.), Some(node(&doc, "b")));
}

#[test]
fn relative_lines_smooth_cubics_and_quadratics_match_equivalent_paths() {
    for (a, b) in [
        ("M10 10h20v20H10z", "M10 10L30 10L30 30L10 30Z"),
        (
            "M10 80C20 50 40 50 50 80S80 110 90 80",
            "M10 80C20 50 40 50 50 80C60 110 80 110 90 80",
        ),
        ("M10 40Q30 20 50 40T90 40", "M10 40Q30 20 50 40Q70 60 90 40"),
        ("m10 40q20-20 40 0t40 0", "M10 40Q30 20 50 40Q70 60 90 40"),
    ] {
        let (_, a) = build(
            &format!("<svg><path d='{a}' fill=none stroke=red stroke-width=4 /></svg>"),
            120,
            140,
        );
        let (_, b) = build(
            &format!("<svg><path d='{b}' fill=none stroke=red stroke-width=4 /></svg>"),
            120,
            140,
        );
        assert!(!a.diagnostics.invalid_geometry && !b.diagnostics.invalid_geometry);
        assert!(image(&a, 120, 140) == image(&b, 120, 140));
    }
}

#[test]
fn endpoint_arcs_apply_sweep_large_arc_and_radius_correction() {
    let (_, s) = build(
        "<svg><path d='M20 50A10 10 0 1 1 80 50A30 30 0 1 1 20 50Z' fill=green /></svg>",
        100,
        100,
    );
    let p = image(&s, 100, 100);
    assert!(!s.diagnostics.invalid_geometry);
    for (x, y) in [(50, 25), (50, 75), (25, 50), (75, 50)] {
        assert_eq!(pixel(&p, 100, x, y), 0x008000);
    }
    assert_eq!(pixel(&p, 100, 50, 15), 0xffffff);
    let (_, s) = build(
        "<svg><path d='M20 50A30 20 30 0140 70L20 50' fill=red /></svg>",
        100,
        100,
    );
    assert!(!s.diagnostics.invalid_geometry);
    assert!(image(&s, 100, 100).contains(&0xff0000));
}

#[test]
fn winding_rules_distinguish_compound_path_holes() {
    let path = "M5 5H95V95H5Z M25 25H75V75H25Z";
    for (rule, expected) in [("nonzero", 0xff0000), ("evenodd", 0xffffff)] {
        let (_, s) = build(
            &format!("<svg><path d='{path}' fill=red fill-rule={rule} /></svg>"),
            100,
            100,
        );
        let p = image(&s, 100, 100);
        assert_eq!(pixel(&p, 100, 50, 50), expected);
        assert_eq!(pixel(&p, 100, 10, 10), 0xff0000);
    }
    let (_, s) = build(
        "<svg><path d='M5 5H95V95H5Z M25 25V75H75V25Z' fill=red /></svg>",
        100,
        100,
    );
    assert_eq!(pixel(&image(&s, 100, 100), 100, 50, 50), 0xffffff);
}

#[test]
fn strokes_observe_caps_joins_miter_limits_and_dashes() {
    for (cap, expected) in [
        ("butt", 0xffffff),
        ("square", 0x0000ff),
        ("round", 0x0000ff),
    ] {
        let (_, s) = build(
            &format!(
                "<svg><path d='M20 20H60' fill=none stroke=blue stroke-width=10 stroke-linecap={cap} /></svg>"
            ),
            90,
            40,
        );
        assert_eq!(pixel(&image(&s, 90, 40), 90, 17, 20), expected);
    }
    for (join, limit, expected) in [
        ("miter", 4, 0x0000ff),
        ("bevel", 4, 0xffffff),
        ("miter", 1, 0xffffff),
    ] {
        let (_, s) = build(
            &format!(
                "<svg><path d='M20 60L40 20L60 60' fill=none stroke=blue stroke-width=12 stroke-linejoin={join} stroke-miterlimit={limit} /></svg>"
            ),
            80,
            80,
        );
        assert_eq!(pixel(&image(&s, 80, 80), 80, 40, 9), expected);
    }
    let (_, s) = build(
        "<svg><path d='M10 20H90' fill=none stroke=blue stroke-width=6 stroke-dasharray='10 10' /></svg>",
        100,
        40,
    );
    let p = image(&s, 100, 40);
    assert_eq!(pixel(&p, 100, 15, 20), 0x0000ff);
    assert_eq!(pixel(&p, 100, 25, 20), 0xffffff);
    assert_eq!(pixel(&p, 100, 35, 20), 0x0000ff);
}

#[test]
fn group_opacity_isolated_before_compositing_overlapping_children() {
    let (_, s) = build(
        "<svg><g opacity=.5 fill=red><rect x=5 y=5 width=50 height=50 /><rect x=25 y=5 width=50 height=50 /></g></svg>",
        90,
        70,
    );
    let p = image(&s, 90, 70);
    near(pixel(&p, 90, 10, 20), 0xff8080);
    near(pixel(&p, 90, 35, 20), 0xff8080);
    near(pixel(&p, 90, 70, 20), 0xff8080);
    let (_, s) = build(
        "<svg><g opacity=.5><rect width=50 height=50 fill=red opacity=.5 /></g></svg>",
        60,
        60,
    );
    near(pixel(&image(&s, 60, 60), 60, 20, 20), 0xffbfbf);
}

#[test]
fn local_linear_radial_gradient_and_href_stops_produce_color_interpolation() {
    let (_, s) = build(
        "<svg><defs><linearGradient id=a><stop offset=0 stop-color=red /><stop offset=1 stop-color=blue /></linearGradient><linearGradient id=b href='#a' /><radialGradient id=r><stop offset=0 stop-color=white /><stop offset=1 stop-color=black /></radialGradient></defs><rect width=100 height=30 fill='url(#b)' /><circle cx=50 cy=75 r=20 fill='url(#r)' /></svg>",
        100,
        100,
    );
    let p = image(&s, 100, 100);
    near(pixel(&p, 100, 24, 15), 0xc1003e);
    near(pixel(&p, 100, 74, 15), 0x4100be);
    assert!(pixel(&p, 100, 50, 75) > 0xe0e0e0);
    assert!(pixel(&p, 100, 50, 93) < 0x303030);
}

#[test]
fn gradients_inherit_color_from_the_resource_and_use_keeps_shadow_inheritance() {
    let (_, scene) = build(
        "<svg><defs color=blue><linearGradient id=base><stop offset=0 style='stop-color:currentColor' /><stop offset=1 stop-color=currentColor /></linearGradient><linearGradient id=copy href='#base' color=green /><rect id=shape width=10 height=10 fill=currentColor /></defs><rect width=30 height=20 color=red fill='url(#base)' /><rect x=30 width=30 height=20 color=red fill='url(#copy)' /><use href='#shape' y=25 color=red /></svg>",
        60,
        40,
    );
    let pixels = image(&scene, 60, 40);
    assert_eq!(pixel(&pixels, 60, 10, 10), 0x0000ff);
    assert_eq!(pixel(&pixels, 60, 40, 10), 0x008000);
    assert_eq!(pixel(&pixels, 60, 5, 30), 0xff0000);
}

#[test]
fn css_cascade_keeps_use_inheritance_and_seeds_color_from_the_html_context() {
    let (_, scene) = build_cascaded(
        "<body style='color:purple'><svg style='color:inherit'><style>.painted {fill:currentColor} .explicit {color:green}</style><defs><rect id=base class=painted width=10 height=10 /><rect id=fixed class='painted explicit' width=10 height=10 /></defs><use href='#base' color=red /><use href='#fixed' x=20 color=red /><rect x=40 width=10 height=10 fill=currentColor /></svg>",
        60,
        20,
    );
    let pixels = image(&scene, 60, 20);
    assert_eq!(pixel(&pixels, 60, 5, 5), 0xff0000);
    assert_eq!(pixel(&pixels, 60, 25, 5), 0x008000);
    assert_eq!(pixel(&pixels, 60, 45, 5), 0x800080);
}

#[test]
fn invalid_style_values_preserve_valid_presentation_geometry_and_paint() {
    let (_, scene) = build_cascaded(
        "<svg><rect width=20 height=20 fill=red style='width:-2px;fill:banana;display:bogus' /></svg>",
        30,
        30,
    );
    let pixels = image(&scene, 30, 30);
    assert_eq!(pixel(&pixels, 30, 10, 10), 0xff0000);
    assert_eq!(pixel(&pixels, 30, 25, 10), 0xffffff);
}

#[test]
fn dashes_join_across_closed_seams_and_zero_length_dashes_keep_round_caps() {
    let (_, closed) = build(
        "<svg><path d='M10 10 H50 V30 H10 Z' fill=none stroke=red stroke-width=6 stroke-dasharray='50 20' /></svg>",
        70,
        45,
    );
    let (_, equivalent) = build(
        "<svg><path d='M40 30 H10 V10 H50 V20' fill=none stroke=red stroke-width=6 /></svg>",
        70,
        45,
    );
    assert_eq!(image(&closed, 70, 45), image(&equivalent, 70, 45));
    let (_, dotted) = build(
        "<svg><path d='M10 10 H40' fill=none stroke=red stroke-width=6 stroke-linecap=round stroke-dasharray='0 10' /></svg>",
        50,
        20,
    );
    let (_, circles) = build(
        "<svg fill=red><circle cx=10 cy=10 r=3 /><circle cx=20 cy=10 r=3 /><circle cx=30 cy=10 r=3 /><circle cx=40 cy=10 r=3 /></svg>",
        50,
        20,
    );
    assert_eq!(image(&dotted, 50, 20), image(&circles, 50, 20));
}

#[test]
fn zero_viewbox_disables_paint_and_object_bbox_clip_percentages_use_unit_box() {
    let (_, empty) = build(
        "<svg viewBox='0 0 0 100'><rect width=50 height=50 fill=red /></svg>",
        60,
        60,
    );
    assert!(image(&empty, 60, 60).iter().all(|&color| color == 0xffffff));
    let (_, scene) = build(
        "<svg><defs><clipPath id=clip clipPathUnits=objectBoundingBox><rect x='25%' y='25%' width='50%' height='50%' /></clipPath></defs><rect x=10 y=10 width=80 height=40 fill=blue clip-path='url(#clip)' /></svg>",
        100,
        60,
    );
    let pixels = image(&scene, 100, 60);
    assert_eq!(pixel(&pixels, 100, 35, 25), 0x0000ff);
    assert_eq!(pixel(&pixels, 100, 25, 25), 0xffffff);
    assert_eq!(pixel(&pixels, 100, 35, 15), 0xffffff);
    assert!(!scene.diagnostics.unsupported, "{:?}", scene.diagnostics);
}

#[test]
fn clipping_and_use_symbol_viewports_preserve_geometry_hit_targets() {
    let (doc, s) = build(
        "<svg><defs><clipPath id=c><circle cx=20 cy=20 r=10 /></clipPath><rect id=base width=10 height=10 fill=blue /><symbol id=icon viewBox='0 0 10 10'><rect width=10 height=10 fill=red /></symbol></defs><rect id=clipped width=40 height=40 fill=green clip-path='url(#c)' /><use id=copy href='#base' x=40 y=10 /><use id=symbol href='#icon' x=50 width=40 height=20 /></svg>",
        100,
        60,
    );
    let p = image(&s, 100, 60);
    assert_eq!(pixel(&p, 100, 20, 20), 0x008000);
    assert_eq!(pixel(&p, 100, 5, 5), 0xffffff);
    assert_eq!(pixel(&p, 100, 45, 15), 0x0000ff);
    assert_eq!(pixel(&p, 100, 65, 10), 0xff0000);
    assert_eq!(pixel(&p, 100, 55, 10), 0xffffff);
    assert_eq!(s.hit_test(20., 20.), Some(node(&doc, "clipped")));
    assert_eq!(s.hit_test(5., 5.), None);
    assert_eq!(s.hit_test(45., 15.), Some(node(&doc, "copy")));
    assert_eq!(s.hit_test(65., 10.), Some(node(&doc, "symbol")));
}

#[test]
fn cascade_overrides_and_explicit_inherit_beat_presentation_attributes() {
    let doc = html::parse(
        "<svg><g id=parent fill=blue><rect id=child fill=red width=30 height=30 /></g></svg>",
    );
    let root = doc.elements("svg").next().unwrap();
    let mut styles = BTreeMap::new();
    let mut parent = SvgStyle::default();
    assert!(parent.parse_property("fill", "green"));
    styles.insert(node(&doc, "parent"), parent);
    let mut child = SvgStyle::default();
    assert!(child.parse_property("fill", "inherit"));
    styles.insert(node(&doc, "child"), child);
    let scene = SvgScene::build(&doc, root, 40., 40., &styles);
    assert_eq!(pixel(&image(&scene, 40, 40), 40, 10, 10), 0x008000);
}

#[test]
fn final_affine_and_callback_clip_write_only_the_owned_pixel_intersection() {
    let (_, s) = build("<svg><rect width=30 height=30 fill=blue /></svg>", 30, 30);
    let mut p = vec![0xffffff; 80 * 80];
    let report = s.paint_transformed(
        &mut p,
        80,
        80,
        [1., 0.5, -0.5, 1., 30., 10.],
        SvgRect {
            x: 0.,
            y: 0.,
            width: 80.,
            height: 80.,
        },
        |x, y| (25. ..45.).contains(&x) && (20. ..50.).contains(&y),
    );
    assert!(!report.invalid_surface && !report.truncated);
    assert!(report.pixels > 0);
    for y in 0..80 {
        for x in 0..80 {
            if !(25..45).contains(&x) || !(20..50).contains(&y) {
                assert_eq!(pixel(&p, 80, x, y), 0xffffff);
            }
        }
    }
}

#[test]
fn malformed_references_depth_and_segment_limits_are_reported_without_native_recursion() {
    std::thread::Builder::new().stack_size(1024*1024).spawn(||{
        let(_,s)=build("<svg><defs><use id=a href='#b' /><use id=b href='#a' /></defs><use href='#a' /></svg>",40,40);assert!(s.diagnostics.unsupported);let _=image(&s,40,40);
        let(_,s)=build(&format!("<svg>{}<circle r=5 />{}</svg>","<g>".repeat(150),"</g>".repeat(150)),40,40);assert!(s.diagnostics.truncated);
        let(_,s)=build(&format!("<svg><path d='M0 0{}' /></svg>","l1 1".repeat(100_000)),40,40);assert!(s.diagnostics.truncated);assert!(s.diagnostics.segments<=rust_browser_engine::svg::MAX_SVG_SEGMENTS);
    }).unwrap().join().unwrap();
}

#[test]
fn surface_and_pixel_work_limits_preserve_the_callers_buffer() {
    let (_, s) = build(
        "<svg><rect width=1000 height=1000 fill=red /><rect width=1000 height=1000 fill=blue /></svg>",
        1000,
        1000,
    );
    let mut short = vec![0x123456; 10];
    let report = s.paint(
        &mut short,
        10,
        10,
        SvgRect {
            x: 0.,
            y: 0.,
            width: 10.,
            height: 10.,
        },
        SvgRect {
            x: 0.,
            y: 0.,
            width: 10.,
            height: 10.,
        },
    );
    assert!(report.invalid_surface);
    assert!(short.iter().all(|&p| p == 0x123456));
    let (_, s) = build(
        &format!(
            "<svg>{}</svg>",
            "<rect width=1000 height=1000 fill=red />".repeat(12)
        ),
        1000,
        1000,
    );
    let mut p = vec![0xffffff; 1_000_000];
    let report = s.paint(
        &mut p,
        1000,
        1000,
        SvgRect {
            x: 0.,
            y: 0.,
            width: 1000.,
            height: 1000.,
        },
        SvgRect {
            x: 0.,
            y: 0.,
            width: 1000.,
            height: 1000.,
        },
    );
    assert!(report.truncated);
    assert!(report.work <= rust_browser_engine::svg::MAX_SVG_RENDER_WORK);
    assert!(report.pixels <= rust_browser_engine::svg::MAX_SVG_PAINT_PIXELS);
}

#[test]
fn presentation_colors_resolve_inherited_custom_properties_before_paint() {
    let (_, scene) = build_cascaded(
        "<style>body { --ink:#9400d3; --nav:#2364aa } .accent { fill:blue }</style><svg><rect width='12' height='12' fill='var(--nav)'/><rect x='12' width='12' height='12' class='accent' fill='var(--nav)'/><g color='var(--ink)' fill='currentColor'><rect x='24' width='12' height='12'/></g><rect x='36' width='12' height='12' fill='var(--missing, red)'/></svg>",
        48,
        12,
    );
    let pixels = image(&scene, 48, 12);
    assert_eq!(
        [
            pixel(&pixels, 48, 5, 5),
            pixel(&pixels, 48, 17, 5),
            pixel(&pixels, 48, 29, 5),
            pixel(&pixels, 48, 41, 5)
        ],
        [0x2364aa, 0x0000ff, 0x9400d3, 0xff0000]
    );
}
