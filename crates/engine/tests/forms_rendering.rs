//! Public form state -> layout -> software pixels. These assert rendering
//! invariants, not a platform-specific pixel golden or full widget conformance.
use rust_browser_engine::{
    Fonts, Layout, LoadedPage, Rect, css::Color, dom::NodeId, layout::DrawCommand, paint::Canvas,
    script::ScriptRuntime,
};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

fn fonts() -> MutexGuard<'static, Fonts> {
    static FONTS: OnceLock<Mutex<Fonts>> = OnceLock::new();
    FONTS
        .get_or_init(|| Mutex::new(Fonts::new().unwrap()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
fn page(source: &str) -> LoadedPage {
    LoadedPage::from_html(
        "https://forms-render.test/",
        &format!(
            "<!doctype html><style>body{{margin:0}}input,textarea,select,button{{font-size:16px}}</style>{source}"
        ),
    )
}
fn id(page: &LoadedPage, name: &str) -> NodeId {
    page.document
        .connected_nodes()
        .find(|&id| page.document.attr(id, "id") == Some(name))
        .unwrap()
}
fn image(layout: &Layout, fonts: &mut Fonts) -> Vec<u32> {
    let mut canvas = Canvas::new(400, 240, Color::WHITE);
    canvas.document(layout, fonts, 0., 0.);
    canvas.pixels
}
fn assert_pixels(actual: &[u32], expected: &[u32], context: &str) {
    assert_eq!(actual.len(), expected.len());
    let changed = actual.iter().zip(expected).filter(|(a, b)| a != b).count();
    assert_eq!(changed, 0, "{context}: {changed} pixels differ");
}
fn painted_control(page: &LoadedPage, layout: &Layout, node: NodeId) -> bool {
    layout.commands.iter().any(|command| {
        matches!(command,
        DrawCommand::Control { control, .. } if Arc::ptr_eq(control, &page.form_controls[&node]))
    })
}

#[test]
fn static_controls_have_snapshots_boxes_hit_targets_and_visible_software_pixels() {
    let mut fonts = fonts();
    let page = page(
        "<input id=text value='Initial text'><input id=password type=password value='secret'>\
         <input id=check type=checkbox checked><input id=radio type=radio checked>\
         <textarea id=area rows=3 cols=12>Initial textarea</textarea>\
         <select id=select><option>First</option><option selected>Second</option></select>\
         <select id=list size=3 multiple><option selected>One</option><option>Two</option></select>\
         <input id=submit type=submit value=Send><button id=button>Button label</button>\
         <input id=hidden type=hidden value='Never visible'><input id=disabled disabled value=Disabled>",
    );
    // No ScriptRuntime has been created: initial markup must paint on its own.
    let layout = Layout::build(&page, &fonts, 400.);
    assert!(!layout.limited);
    for name in [
        "text", "password", "check", "radio", "area", "select", "list", "submit", "button",
        "disabled",
    ] {
        let node = id(&page, name);
        assert!(page.form_controls.contains_key(&node));
        let rect = layout
            .rect_for(node)
            .unwrap_or_else(|| panic!("missing box for {name}"));
        assert!(
            rect.x.is_finite() && rect.y.is_finite() && rect.width > 0. && rect.height > 0.,
            "invalid {name}: {rect:?}"
        );
        assert_eq!(
            layout.element_at(rect.x + rect.width / 2., rect.y + rect.height / 2.),
            Some(node),
            "hit target {name}"
        );
        if name != "button" {
            assert!(
                painted_control(&page, &layout, node),
                "no widget command for {name}"
            );
        }
    }
    let text = id(&page, "text");
    let textarea = id(&page, "area");
    assert!(layout.rect_for(textarea).unwrap().height > layout.rect_for(text).unwrap().height * 2.);
    assert!(layout.rect_for(id(&page, "hidden")).is_none());
    assert!(!painted_control(&page, &layout, id(&page, "hidden")));
    assert!(page.form_controls[&id(&page, "check")].checked);
    assert!(page.form_controls[&id(&page, "disabled")].disabled);
    let choices = &page.form_controls[&id(&page, "select")].options;
    assert_eq!(choices.iter().filter(|option| option.selected).count(), 1);
    assert_eq!(
        choices.iter().find(|option| option.selected).unwrap().label,
        "Second"
    );
    assert!(
        image(&layout, &mut fonts)
            .iter()
            .any(|&pixel| pixel != 0x00ff_ffff)
    );
}

#[test]
fn live_dirty_values_checks_and_option_selection_reach_immutable_paint_snapshots() {
    let mut fonts = fonts();
    let mut page = page(
        "<input id=text value=Original><textarea id=area>Original textarea</textarea>\
         <input id=check type=checkbox><select id=select><option>First</option><option>Second</option></select>",
    );
    let text = id(&page, "text");
    let area = id(&page, "area");
    let check = id(&page, "check");
    let select = id(&page, "select");
    let initial = Layout::build(&page, &fonts, 400.);
    let before = image(&initial, &mut fonts);
    let old = page.form_controls[&text].clone();
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    runtime
        .eval(
            "document.getElementById('text').value='Edited live';\
         document.getElementById('area').value='New\\nlines';\
         document.getElementById('check').checked=true;\
         document.getElementById('select').selectedIndex=1;",
        )
        .unwrap();
    assert!(runtime.snapshot(&mut page));
    assert_eq!(page.document.attr(text, "value"), Some("Original"));
    assert_eq!(page.document.text(area), "Original textarea");
    assert!(page.document.attr(check, "checked").is_none());
    assert_eq!(page.form_controls[&text].value, "Edited live");
    assert_eq!(page.form_controls[&area].value, "New\nlines");
    assert!(page.form_controls[&check].checked);
    assert!(page.form_controls[&select].options[1].selected);
    assert_eq!(old.value, "Original");
    assert!(!Arc::ptr_eq(&old, &page.form_controls[&text]));
    let current = Layout::build(&page, &fonts, 400.);
    assert!(image(&current, &mut fonts) != before);
    assert_pixels(
        &image(&initial, &mut fonts),
        &before,
        "publishing a new snapshot mutated an existing layout",
    );
    for node in [text, area, check, select] {
        assert!(painted_control(&page, &current, node));
    }
}

fn input_layout(
    kind: &str,
    value: &str,
    display: &str,
    deep: bool,
    fonts: &Fonts,
) -> (LoadedPage, Layout) {
    let opening = if deep {
        "<div>".repeat(80)
    } else {
        String::new()
    };
    let closing = if deep {
        "</div>".repeat(80)
    } else {
        String::new()
    };
    let page = page(&format!(
        "{opening}<input id=field type={kind} value='{value}' style='display:{display};width:220px;height:28px'>{closing}"
    ));
    let layout = Layout::build(&page, fonts, 400.);
    (page, layout)
}

#[test]
fn passwords_are_masked_under_author_display_overrides_and_layout_fallback() {
    let mut fonts = fonts();
    for (display, deep) in [
        ("inline-block", false),
        ("inline", false),
        ("block", false),
        ("inline-block", true),
        ("inline", true),
    ] {
        let (first_page, first) = input_layout("password", "SECRET", display, deep, &fonts);
        let (_, second) = input_layout("password", "BANANA", display, deep, &fonts);
        assert_eq!(first.limited, deep);
        let node = id(&first_page, "field");
        assert!(
            first.rect_for(node).is_some(),
            "password lost its box: {display} deep={deep}"
        );
        let masked = image(&first, &mut fonts);
        assert_pixels(
            &masked,
            &image(&second, &mut fonts),
            &format!("same-length password contents changed pixels: {display} deep={deep}"),
        );
        let (_, bullets) = input_layout("text", "••••••", display, deep, &fonts);
        assert_pixels(
            &masked,
            &image(&bullets, &mut fonts),
            &format!("password pixels must contain only the masked glyphs: {display} deep={deep}"),
        );
        let (_, plain_first) = input_layout("text", "SECRET", display, deep, &fonts);
        let (_, plain_second) = input_layout("text", "BANANA", display, deep, &fonts);
        assert!(
            image(&plain_first, &mut fonts) != image(&plain_second, &mut fonts),
            "text controls prove this comparison is sensitive to leaked glyphs"
        );
    }
}

#[test]
fn layout_fallback_uses_live_values_instead_of_stale_content_attributes() {
    let mut fonts = fonts();
    for kind in ["text", "password"] {
        let (mut live_page, _) = input_layout(kind, "Original", "inline-block", true, &fonts);
        let mut runtime = ScriptRuntime::new(&live_page).unwrap();
        runtime
            .eval("document.getElementById('field').value='Edited live';")
            .unwrap();
        assert!(runtime.snapshot(&mut live_page));
        assert_eq!(
            live_page.document.attr(id(&live_page, "field"), "value"),
            Some("Original")
        );
        let live = Layout::build(&live_page, &fonts, 400.);
        let (_, expected) = input_layout(kind, "Edited live", "inline-block", true, &fonts);
        assert!(live.limited && expected.limited);
        assert_pixels(
            &image(&live, &mut fonts),
            &image(&expected, &mut fonts),
            &format!("fallback rendered the content attribute rather than the live {kind} value"),
        );
    }
}

#[test]
fn live_password_value_is_masked_without_mutating_its_content_attribute() {
    let mut fonts = fonts();
    let mut first_page =
        page("<input id=password type=password value=Original style='width:220px;height:28px'>");
    let mut second_page = first_page.clone();
    let mut first_runtime = ScriptRuntime::new(&first_page).unwrap();
    let mut second_runtime = ScriptRuntime::new(&second_page).unwrap();
    first_runtime
        .eval("document.getElementById('password').value='SECRET';")
        .unwrap();
    second_runtime
        .eval("document.getElementById('password').value='BANANA';")
        .unwrap();
    first_runtime.snapshot(&mut first_page);
    second_runtime.snapshot(&mut second_page);
    assert_eq!(
        first_page
            .document
            .attr(id(&first_page, "password"), "value"),
        Some("Original")
    );
    let first = Layout::build(&first_page, &fonts, 400.);
    let second = Layout::build(&second_page, &fonts, 400.);
    assert_pixels(
        &image(&first, &mut fonts),
        &image(&second, &mut fonts),
        "live password masking",
    );
}

#[test]
fn hidden_inputs_stay_unrendered_despite_author_display_and_fallback() {
    let fonts = fonts();
    for deep in [false, true] {
        let opening = if deep {
            "<div>".repeat(80)
        } else {
            String::new()
        };
        let closing = if deep {
            "</div>".repeat(80)
        } else {
            String::new()
        };
        let page = page(&format!(
            "{opening}<input id=hidden type=HiDdEn value='Must never paint' style='display:block!important;width:200px;height:30px'><p>Visible text</p>{closing}"
        ));
        let layout = Layout::build(&page, &fonts, 400.);
        assert_eq!(layout.limited, deep);
        let hidden = id(&page, "hidden");
        assert!(
            layout.rect_for(hidden).is_none(),
            "hidden input acquired a layout box, deep={deep}"
        );
        assert!(!painted_control(&page, &layout, hidden));
    }
}

#[test]
fn control_paint_respects_viewport_clip_and_restores_clip_for_later_commands() {
    let mut fonts = fonts();
    let page = page(
        "<input value='Long overflowing text long overflowing text'><input type=password value=SECRET>\
         <input type=checkbox checked><input type=radio checked>\
         <textarea>first line\nsecond line\nthird line</textarea>\
         <select><option selected>Selected label</option></select>\
         <select multiple size=3><option selected>One</option><option>Two</option></select>",
    );
    let layout = Layout::build(&page, &fonts, 400.);
    let commands: Vec<_> = layout
        .commands
        .iter()
        .filter(|command| matches!(command, DrawCommand::Control { .. }))
        .cloned()
        .collect();
    assert_eq!(commands.len(), 7);
    let clip = Rect {
        x: 32.,
        y: 16.,
        width: 96.,
        height: 72.,
    };
    let background = Color(27, 31, 49, 255);
    let packed = 0x001b_1f31;
    for command in commands {
        let mut only = layout.clone();
        let DrawCommand::Control {
            node,
            control,
            style,
            ..
        } = command
        else {
            unreachable!()
        };
        only.commands = vec![
            DrawCommand::Control {
                node,
                rect: Rect {
                    x: 20.,
                    y: 4.,
                    width: 70.,
                    height: 50.,
                },
                control,
                style,
            },
            DrawCommand::Rect {
                rect: Rect {
                    x: 104.,
                    y: 64.,
                    width: 8.,
                    height: 8.,
                },
                color: Color(11, 199, 61, 255),
            },
        ];
        let mut canvas = Canvas::new(160, 100, background);
        canvas.clip(clip);
        canvas.document(&only, &mut fonts, 0., 0.);
        assert_eq!(
            canvas.pixels[68 * 160 + 108],
            0x000b_c73d,
            "control left its private clip installed"
        );
        for y in 0..100 {
            for x in 0..160 {
                if !clip.contains(x as f32, y as f32) {
                    assert_eq!(
                        canvas.pixels[y * 160 + x],
                        packed,
                        "paint escaped viewport at{x},{y}"
                    );
                }
            }
        }
        assert!(
            canvas
                .pixels
                .iter()
                .enumerate()
                .any(|(index, &pixel)| pixel != packed && index % 160 < 90),
            "widget pixels were not drawn in visible intersection"
        );
    }
}

#[test]
fn zoomed_and_scrolled_control_pixels_stay_within_the_active_viewport() {
    let mut fonts = fonts();
    let page = page(
        "<textarea rows=5 style='width:150px;height:100px'>First line\nSecond line\nThird line</textarea><input type=password value=SECRET>",
    );
    let layout = Layout::build(&page, &fonts, 400.);
    let clip = Rect {
        x: 11.,
        y: 17.,
        width: 130.,
        height: 65.,
    };
    for (scale, scroll, offset) in [(1., 10., 5.), (1.5, 8., 3.), (2., 25., 7.)] {
        let mut canvas = Canvas::new(180, 120, Color(27, 31, 49, 255));
        canvas.clip(clip);
        canvas.document_scaled(&layout, &mut fonts, offset, scroll, scale);
        let mut changed = 0;
        for y in 0..120 {
            for x in 0..180 {
                let pixel = canvas.pixels[y * 180 + x];
                if !clip.contains(x as f32, y as f32) {
                    assert_eq!(
                        pixel, 0x001b_1f31,
                        "zoomed paint escaped viewport at{x},{y}"
                    );
                } else if pixel != 0x001b_1f31 {
                    changed += 1;
                }
            }
        }
        assert!(
            changed > 0,
            "zoom/scroll test produced no visible widget pixels"
        );
    }
}

#[test]
fn single_line_text_centers_vertically_and_textarea_stays_top_aligned() {
    let mut fonts = fonts();
    let mut ink_rows = |tag: &str, height: u32| {
        let style = format!(
            "display:block;margin:0;padding:0;border:none;color:black;\
                             background:white;width:100px;height:{height}px;line-height:24px"
        );
        let sample = if tag == "input" {
            page(&format!("<input value=test style='{style}'>"))
        } else {
            page(&format!("<textarea style='{style}'>test</textarea>"))
        };
        let layout = Layout::build(&sample, &fonts, 160.);
        let mut canvas = Canvas::new(160, 100, Color::WHITE);
        canvas.document(&layout, &mut fonts, 0., 0.);
        let rows: Vec<_> = canvas
            .pixels
            .chunks_exact(160)
            .take(height as usize)
            .enumerate()
            .filter(|(_, row)| row[..100].iter().any(|&px| px < 0x707070))
            .map(|(y, _)| y)
            .collect();
        (*rows.first().unwrap(), *rows.last().unwrap())
    };
    let (top, bottom) = ink_rows("input", 24);
    assert_eq!(ink_rows("input", 64), (top + 20, bottom + 20));
    assert_eq!(ink_rows("textarea", 64), ink_rows("textarea", 24));
}

#[test]
fn controls_paint_the_final_min_or_max_constrained_content_box() {
    let mut fonts = fonts();
    for (rule, reference) in [
        ("min-height:52px", "height:52px"),
        ("height:90px;max-height:36px", "height:36px"),
        ("min-height:52px;max-height:36px", "height:52px"),
    ] {
        for (tag, sizing, deep) in [
            ("input", "content-box", false),
            ("input", "border-box", false),
            ("input", "border-box", true),
            ("textarea", "border-box", false),
        ] {
            let render = |height: &str, fonts: &mut Fonts| {
                let box_style = format!(
                    "display:block;width:112px;box-sizing:{sizing};{height};padding:3px 5px 7px;border:2px solid black"
                );
                let field = if tag == "input" {
                    format!("<input id=field value=test style='{box_style}'>")
                } else {
                    format!("<textarea id=field rows=1 style='{box_style}'>test</textarea>")
                };
                let source = if deep {
                    format!("{}{}{}", "<div>".repeat(80), field, "</div>".repeat(80))
                } else {
                    field
                };
                let page = page(&source);
                let layout = Layout::build(&page, fonts, 400.);
                let field = id(&page, "field");
                let outer = layout.rect_for(field).unwrap();
                let content = layout
                    .commands
                    .iter()
                    .find_map(|cmd| match cmd {
                        DrawCommand::Control { node, rect, .. } if *node == field => Some(rect),
                        _ => None,
                    })
                    .unwrap();
                assert_eq!(
                    content.height,
                    outer.height - 14.,
                    "{height} {sizing} in {tag}"
                );
                image(&layout, fonts)
            };
            assert_pixels(
                &render(rule, &mut fonts),
                &render(reference, &mut fonts),
                &format!("{rule} ({tag}, {sizing}, deep={deep})"),
            );
        }
    }
}
