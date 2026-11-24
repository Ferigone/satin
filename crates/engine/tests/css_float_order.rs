use rust_browser_engine::{Fonts, Layout, LoadedPage, Rect, layout::Viewport, paint::Canvas};

#[test]
fn static_float_paints_and_hits_between_block_and_inline_content() {
    let mut fonts = Fonts::new().unwrap();
    for stage_style in ["", "position:relative", "position:relative; z-index:0"] {
        let page = LoadedPage::from_html(
            "https://float-order.test/",
            &format!(
                r#"<!doctype html>
                <style>
                    body {{ margin: 0; font-size: 0; line-height: 0; }}
                    #float {{ float: left; width: 40px; height: 20px; background: #f00; }}
                    #block {{ width: 200px; height: 20px; background: #00f; }}
                    #inline {{ display: inline-block; width: 20px; height: 20px;
                               margin-left: -20px; background: #0f0; }}
                    #ink {{ width: 5px; height: 20px; background: #f0f; }}
                    #behind, #above {{ position: absolute; top: 0; height: 20px;
                                       background: #ff0; }}
                    #behind {{ left: 0; width: 8px; z-index: -1; }}
                    #above {{ left: 30px; width: 10px; }}
                </style>
                <div style="{stage_style}">
                    <div id="float"><div id="behind"></div><div id="above"></div></div>
                    <div id="block"><span id="inline"><span id="ink" style="display:block"></span></span></div>
                </div>"#
            ),
        );
        let layout = Layout::build_with_viewport(
            &page,
            &fonts,
            Viewport {
                width: 220.,
                height: 30.,
                ..Viewport::default()
            },
        );
        let mut canvas = Canvas::new(220, 30, layout.background);
        canvas.document(&layout, &mut fonts, 0., 0.);
        assert!(!layout.limited && !canvas.limited, "{stage_style}");

        let node = |name: &str| {
            page.document
                .connected_nodes()
                .find(|&id| page.document.attr(id, "id") == Some(name))
                .unwrap()
        };
        for (name, width) in [("float", 40.), ("block", 200.)] {
            assert_eq!(
                layout.rect_for(node(name)),
                Some(Rect {
                    x: 0.,
                    y: 0.,
                    width,
                    height: 20.,
                }),
                "{stage_style}: {name} must stay at its flow position",
            );
        }
        for (x, rgb, target) in [
            (4, 0xff0000, "float"), // The negative-z descendant escapes below the float.
            (12, 0xff0000, "float"),
            (22, 0xff00ff, "ink"), // The inline-block's child paints with it.
            (27, 0x00ff00, "inline"),
            (35, 0xffff00, "above"), // The positioned auto-z descendant escapes above inlines.
            (60, 0x0000ff, "block"),
        ] {
            assert_eq!(canvas.pixels[10 * 220 + x], rgb, "{stage_style}: x={x}");
            assert_eq!(
                layout.element_at(x as f32, 10.),
                Some(node(target)),
                "{stage_style}: x={x}",
            );
        }
    }
}
