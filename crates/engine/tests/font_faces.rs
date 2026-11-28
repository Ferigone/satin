use rust_browser_engine::{
    Fonts, Layout, LoadedPage,
    css::{self, Color},
    fonts::{FontFace, FontFaceEntry, FontFamily, FontFormat, FontSet, FontSource, FontStyle},
    layout::DrawCommand,
    paint::Canvas,
};
use std::sync::Arc;

const WIDE: &[u8] = include_bytes!("fixtures/fonts/wide.ttf");
const WOFF: &[u8] = include_bytes!("fixtures/fonts/wide.woff");
const WOFF2: &[u8] = include_bytes!("fixtures/fonts/wide.woff2");
const FALLBACK: &[u8] = include_bytes!("fixtures/fonts/fallback.ttf");
const VARIABLE: &[u8] = include_bytes!("fixtures/fonts/variable.woff2");

fn assert_close(actual: f32, expected: f32) {
    assert!((actual - expected).abs() < 0.05, "{actual} != {expected}");
}

fn font_set(css: &str, files: &[&[u8]]) -> FontSet {
    let rules = css::font_faces(css, 800.);
    assert_eq!(rules.len(), files.len());
    FontSet::new(
        rules
            .into_iter()
            .zip(files)
            .map(|(rule, bytes)| FontFaceEntry {
                rule,
                face: FontFace::decode(bytes).unwrap(),
            })
            .collect(),
    )
    .unwrap()
}

#[test]
fn font_face_rule_parses_cascade_lists_and_unicode_ranges() {
    let rules = css::font_faces(
        r#"
        @font-face {
            font-family: "Audit, Face";
            src: local('Missing'), url("relative/font.woff2?cache=1") format("woff2"), url(fallback.ttf) format(truetype);
            font-style: italic;
            font-weight: 300 650;
            unicode-range: U+0041, U+04??, U+1F300-1F5FF;
            font-display: swap;
        }
        @media (min-width: 900px) { @font-face { font-family: Not Active; src: url(x); } }
        @font-face { font-family: serif; src: url(generic); }
    "#,
        800.,
    );
    assert_eq!(rules.len(), 1);
    let rule = &rules[0];
    assert_eq!(rule.family.as_ref(), "Audit, Face");
    assert_eq!(rule.weight, [300, 650]);
    assert_eq!(rule.style, FontStyle::Italic);
    assert_eq!(
        rule.unicode_ranges
            .iter()
            .map(|r| (r.start, r.end))
            .collect::<Vec<_>>(),
        [(65, 65), (0x400, 0x4ff), (0x1f300, 0x1f5ff)]
    );
    assert!(matches!(&rule.sources[0], FontSource::Local(name) if name.as_ref() == "Missing"));
    assert!(
        matches!(&rule.sources[1], FontSource::Url { url, format: Some(FontFormat::Woff2) } if url.as_ref() == "relative/font.woff2?cache=1")
    );
    assert!(
        matches!(&rule.sources[2], FontSource::Url { url, format: Some(FontFormat::TrueType) } if url.as_ref() == "fallback.ttf")
    );
    let rules = css::font_faces(
        r#"
        @font-face{font-family:Test;src:url(wrong url), url (skip.woff2),
          url(escape\20 name.woff2) format("woff2") tech(variations), url(color) tech(color-COLRv1)}
    "#,
        800.,
    );
    assert_eq!(rules[0].sources.len(), 2);
    assert!(
        matches!(&rules[0].sources[0],FontSource::Url{url,..} if url.as_ref()=="escape name.woff2")
    );
    assert!(matches!(
        &rules[0].sources[1],
        FontSource::Url {
            format: Some(FontFormat::Unsupported),
            ..
        }
    ));
}

#[test]
fn css_block_recovery_keeps_later_font_descriptors_and_rules() {
    let rules = css::font_faces(
        r#"
        @font-face {
            font-family: First;
            unknown: fn(});
            src: url(first.woff2);
        }
        @font-face {
            font-family: Second;
            unknown: [}];
            src: URL([part\).woff2)
        }
        @font-face { font-family: Later; src: url(later.woff2); }
    "#,
        800.,
    );
    let families: Vec<_> = rules.iter().map(|r| r.family.as_ref()).collect();
    assert_eq!(families, ["First", "Second", "Later"]);
    assert!(matches!(
        &rules[1].sources[0],
        FontSource::Url { url, .. } if url.as_ref() == "[part).woff2"
    ));
}

#[test]
fn family_weight_and_style_are_real_inherited_css_values() {
    let page = LoadedPage::from_html(
        "https://fonts.test/",
        r#"
        <style>#p { font: italic 500 20px/1.2 "Audit, Face", monospace; }
        #i { font-weight: bolder; font-family: inherit; font-family: ; }
        #j { font-weight: lighter; font-style: normal; font-family: "serif", serif; }
        #heavy {font-weight:950} #h {font-weight:bolder; font-family:"123"}
        #light {font-weight:60} #l {font-weight:lighter; font-family:"initial"}</style>
        <div id=p><span id=i>A</span><span id=j>B</span></div>
        <div id=heavy><span id=h>A</span></div><div id=light><span id=l>A</span></div>"#,
    );
    let fonts = Fonts::new().unwrap();
    let layout = Layout::build(&page, &fonts, 400.);
    let style = |name| {
        let id = page
            .document
            .connected_nodes()
            .find(|&id| page.document.attr(id, "id") == Some(name))
            .unwrap();
        &layout.computed_styles[id.0]
    };
    assert_eq!(style("p").font_family_css(), "\"Audit, Face\", monospace");
    assert_eq!(style("i").font_family_css(), style("p").font_family_css());
    assert_eq!(style("i").font_weight, 700);
    assert_eq!(style("i").font_style, FontStyle::Italic);
    assert_eq!(style("j").font_weight, 100);
    assert_eq!(style("j").font_style, FontStyle::Normal);
    assert_eq!(style("j").font_family_css(), "\"serif\", serif");
    assert_eq!(style("h").font_weight, 950);
    assert_eq!(style("h").font_family_css(), "\"123\"");
    assert_eq!(style("l").font_weight, 60);
    assert_eq!(style("l").font_family_css(), "\"initial\"");
}

#[test]
fn ttf_woff_and_woff2_decode_to_the_same_metrics_and_glyphs() {
    let fonts = Fonts::new().unwrap();
    let request = rust_browser_engine::fonts::FontRequest {
        families: Arc::from([FontFamily::Named("Audit".into())]),
        weight: 400,
        style: FontStyle::Normal,
    };
    let source = "@font-face {font-family:Audit;src:url(face)}";
    let mut glyph_id = None;
    for bytes in [WIDE, WOFF, WOFF2] {
        let set = font_set(source, &[bytes]);
        let run = fonts.shape_text("AAA", 20., &request, &set);
        assert_close(run.advance, 48.);
        assert_eq!(run.faces().len(), 1);
        assert_eq!(
            run.face_for(&run.glyphs[0]).face().id(),
            set.entries()[0].face.id()
        );
        assert!(run.face_for(&run.glyphs[0]).face().is_web());
        assert!(
            run.glyphs
                .iter()
                .all(|g| g.id == run.glyphs[0].id && g.id != 0)
        );
        assert_eq!(*glyph_id.get_or_insert(run.glyphs[0].id), run.glyphs[0].id);
    }
}

#[test]
fn matching_uses_ordered_families_face_descriptors_ranges_and_grapheme_coverage() {
    let fonts = Fonts::new().unwrap();
    let set = font_set(
        "
        @font-face {font-family:Primary;src:url(wide);unicode-range:U+0041; font-weight:400}
        @font-face {font-family:Primary;src:url(narrow);unicode-range:U+0042; font-weight:400}
        @font-face {font-family:Primary;src:url(narrow);font-weight:700}
        @font-face {font-family:Fallback;src:url(narrow)}
    ",
        &[WIDE, FALLBACK, FALLBACK, FALLBACK],
    );
    let mut style = css::Style::default();
    style.font_family = css::font_families("Missing, Primary, Fallback")
        .unwrap()
        .into();
    let normal = fonts.shape_text("AB", 20., &style.font_request(), &set);
    assert_close(normal.advance, 21.);
    assert_eq!(normal.faces().len(), 2);
    assert_eq!(
        normal.face_for(&normal.glyphs[0]).face().id(),
        set.entries()[0].face.id()
    );
    assert_eq!(
        normal.face_for(&normal.glyphs[1]).face().id(),
        set.entries()[1].face.id()
    );
    let mark = fonts.shape_text("A\u{301}", 20., &style.font_request(), &set);
    assert!(mark.glyphs.len() >= 2);
    assert!(
        mark.glyphs
            .iter()
            .all(|g| mark.face_for(g).face().id() == set.entries()[3].face.id())
    );
    style.font_weight = 700;
    let bold = fonts.shape_text("A", 20., &style.font_request(), &set);
    assert_close(bold.advance, 10.);
    assert_eq!(
        bold.face_for(&bold.glyphs[0]).face().id(),
        set.entries()[2].face.id()
    );
}

#[test]
fn layout_runs_keep_exact_downloaded_face_for_independent_painter_and_release_it() {
    let fonts = Fonts::new().unwrap();
    let mut page = LoadedPage::from_html(
        "https://fonts.test/",
        r#"
        <style>body{margin:0} #probe{font:40px/40px Audit}</style><span id=probe>A</span>"#,
    );
    let set = font_set("@font-face{font-family:Audit;src:url(face)}", &[WOFF2]);
    let weak = set.entries()[0].face.downgrade();
    let id = set.entries()[0].face.id();
    page.fonts = Arc::new(set);
    let layout = Layout::build(&page, &fonts, 200.);
    let run = layout
        .commands
        .iter()
        .find_map(|command| match command {
            DrawCommand::Text { run, .. }
                if run.glyphs.iter().any(|g| run.face_for(g).face().id() == id) =>
            {
                Some(run)
            }
            _ => None,
        })
        .expect("downloaded face used by actual layout");
    assert_close(run.advance, 32.);
    drop(page);
    assert!(weak.upgrade().is_some());
    let mut painter_fonts = Fonts::new().unwrap();
    let mut canvas = Canvas::new(80, 50, Color::WHITE);
    canvas.document(&layout, &mut painter_fonts, 0., 0.);
    assert_eq!(canvas.pixels[15 * 80 + 25], 0);
    assert_eq!(canvas.pixels[15 * 80 + 34], 0xffffff);
    assert!(!canvas.limited);
    drop(layout);
    assert!(
        weak.upgrade().is_none(),
        "shape or bitmap cache retained a dead page face"
    );
}

#[test]
fn variable_weight_uses_same_coordinates_for_advance_and_outline() {
    let mut fonts = Fonts::new().unwrap();
    let set = font_set(
        "@font-face{font-family:Audit;src:url(face);font-weight:400 700}",
        &[VARIABLE],
    );
    let mut style = css::Style::default();
    style.font_family = css::font_families("Audit").unwrap().into();
    let regular = fonts.shape_text("A", 40., &style.font_request(), &set);
    style.font_weight = 550;
    let middle = fonts.shape_text("A", 40., &style.font_request(), &set);
    style.font_weight = 700;
    let bold = fonts.shape_text("A", 40., &style.font_request(), &set);
    assert_close(regular.advance, 16.);
    assert_close(middle.advance, 24.);
    assert_close(bold.advance, 32.);
    assert_eq!(bold.faces()[0].variation_weight(), Some(700));
    let mut canvas = Canvas::new(80, 70, Color::WHITE);
    canvas.text(0., 30., &regular, Color::BLACK, &mut fonts);
    canvas.text(0., 60., &bold, Color::BLACK, &mut fonts);
    assert_eq!(canvas.pixels[20 * 80 + 24], 0xffffff);
    assert_eq!(canvas.pixels[50 * 80 + 24], 0);
}

#[test]
fn font_weight_descriptor_normalizes_descending_variable_ranges() {
    let fonts = Fonts::new().unwrap();
    let request = rust_browser_engine::fonts::FontRequest {
        families: Arc::from([FontFamily::Named("Audit".into())]),
        weight: 550,
        style: FontStyle::Normal,
    };
    for descriptor in [
        "400 700",
        "700 400",
        "bold normal",
        "400 700; font-weight: 700 0",
    ] {
        let set = font_set(
            &format!("@font-face{{font-family:Audit;src:url(face);font-weight:{descriptor}}}"),
            &[VARIABLE],
        );
        assert_eq!(set.entries()[0].rule.weight, [400, 700], "{descriptor}");
        let run = fonts.shape_text("A", 40., &request, &set);
        assert_close(run.advance, 24.);
        assert_eq!(run.faces()[0].variation_weight(), Some(550), "{descriptor}");
    }
}

#[test]
fn invalid_unicode_range_preserves_the_previous_descriptor_and_glyph_fallback() {
    let fonts = Fonts::new().unwrap();
    let request = rust_browser_engine::fonts::FontRequest {
        families: Arc::from([
            FontFamily::Named("Audit".into()),
            FontFamily::Named("AuditFallback".into()),
        ]),
        weight: 400,
        style: FontStyle::Normal,
    };
    for (value, uses_wide) in [
        ("U+40-110000", false),
        ("U+??????", false),
        ("U+0000041", false),
        ("U+41-0000042", false),
        ("U++41", false),
        ("U+41, U+00-110000", false),
        ("U+42-41", false),
        ("U+0041-10FFFF", true),
        ("u+4?", true),
        ("U+??", true),
        ("U+40, U+41", true),
    ] {
        let set = font_set(
            &format!(
                "@font-face{{font-family:Audit;src:url(wide);unicode-range:U+42;unicode-range:{value}}}
                 @font-face{{font-family:AuditFallback;src:url(fallback)}}"
            ),
            &[WIDE, FALLBACK],
        );
        let run = fonts.shape_text("A", 40., &request, &set);
        let picked = run.face_for(&run.glyphs[0]).face().id();
        let expected = &set.entries()[if uses_wide { 0 } else { 1 }];
        assert_eq!(picked, expected.face.id(), "unicode-range: {value}");
        assert_close(run.advance, if uses_wide { 32. } else { 20. });
    }
}

#[test]
fn decoder_rejects_truncation_and_declared_expansion_before_codec_work() {
    for bytes in [WOFF, WOFF2] {
        assert!(FontFace::decode(&bytes[..bytes.len() - 3]).is_err());
        let mut bad = bytes.to_vec();
        bad[16..20].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(FontFace::decode(&bad).is_err());
    }
    let mut woff = WOFF.to_vec();
    woff[56..60].copy_from_slice(&u32::MAX.to_be_bytes());
    assert!(FontFace::decode(&woff).is_err());
    assert!(FontFace::decode(b"ttcf\0\0\0\0\0\0\0\0").is_err());
}

#[test]
fn compound_glyph_cycle_is_rejected_before_a_rasterizer_can_recurse() {
    let mut bytes = WIDE.to_vec();
    let table = |name: &[u8; 4]| {
        let count = u16::from_be_bytes(WIDE[4..6].try_into().unwrap()) as usize;
        let record = WIDE[12..12 + count * 16]
            .as_chunks::<16>()
            .0
            .iter()
            .find(|record| &record[..4] == name)
            .unwrap();
        u32::from_be_bytes(record[8..12].try_into().unwrap()) as usize
    };
    let head = table(b"head");
    let loca = table(b"loca");
    let long = WIDE[head + 50..head + 52] == [0, 1];
    let glyph = if long {
        u32::from_be_bytes(WIDE[loca + 8..loca + 12].try_into().unwrap()) as usize
    } else {
        u16::from_be_bytes(WIDE[loca + 4..loca + 6].try_into().unwrap()) as usize * 2
    };
    let glyph = table(b"glyf") + glyph;
    bytes[glyph..glyph + 2].copy_from_slice(&(-1_i16).to_be_bytes());
    bytes[glyph + 10..glyph + 14].copy_from_slice(&[0, 0, 0, 2]);
    assert!(
        FontFace::decode(&bytes)
            .unwrap_err()
            .0
            .contains("Cyclic glyph components")
    );
}


// Generated text is a layout child, not a DOM child. Icon fonts often use
// exactly this selector shape and a CSS hexadecimal escape for the glyph.
#[test]
fn generated_icon_text_uses_downloaded_face_without_mutating_dom() {
    let fonts = Fonts::new().unwrap();
    let mut page = LoadedPage::from_html(
        "https://fonts.test/",
        r#"
        <style>
          body { margin: 0; }
          .control { display: grid; width: 40px; height: 40px; place-content: center; }
          .bi::before, [class*=" bi-"]::before {
            display: inline-block; font-family: Audit !important;
            font-size: 20px; font-style: normal; font-weight: 400;
          }
          .bi-plus::before { content: "\0041"; }
        </style>
        <a class=control><i class="bi bi-plus"></i></a>
        "#,
    );
    let set = font_set("@font-face{font-family:Audit;src:url(face)}", &[WOFF2]);
    let face_id = set.entries()[0].face.id();
    page.fonts = Arc::new(set);
    let icon = page.document.elements("i").next().unwrap();
    assert!(page.document.nodes[icon.0].children.is_empty());
    let layout = Layout::build(&page, &fonts, 200.);
    let run = layout.commands.iter().find_map(|command| match command {
        DrawCommand::Text { run, .. }
            if run.glyphs.iter().any(|g| run.face_for(g).face().id() == face_id) => Some(run),
        _ => None,
    }).expect("generated ::before icon must reach the text painter with the downloaded face");
    assert_close(run.advance, 16.);
    assert!(page.document.nodes[icon.0].children.is_empty());
}
