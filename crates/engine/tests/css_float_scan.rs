use rust_browser_engine::{Fonts, Layout, LoadedPage};

const ROWS: usize = 300;
const LEFT_ROW: &str = "<div class=left></div><i></i><div class=clear-left></div>";
const RIGHT_ROW: &str = "<div class=right></div><i></i><div class=clear-right></div>";

fn check_rows(body: &str, chip_x: f32) {
    let html = format!(
        "<!doctype html><style>\
         body {{ margin: 0; font-size: 0; line-height: 0 }}\
         .left, .right {{ width: 40px; height: 20px }}\
         .left, .tall {{ float: left }} .right {{ float: right }}\
         .tall {{ width: 15px; height: 7000px }}\
         i {{ display: inline-block; width: 10px; height: 10px }}\
         .clear-left, .clear-right {{ height: 1px }}\
         .clear-left {{ clear: left }} .clear-right {{ clear: right }}\
         </style>{body}"
    );
    let page = LoadedPage::from_html("https://example.test/", &html);
    let fonts = Fonts::new().unwrap();
    let layout = Layout::build(&page, &fonts, 100.);
    assert!(!layout.limited, "expired floats exhausted the scan budget");

    let chips: Vec<_> = page.document.elements("i").collect();
    assert_eq!(chips.len(), ROWS);
    for (row, chip) in chips.into_iter().enumerate() {
        let rect = layout.flow_rect_for(chip).expect("inline-block chip box");
        assert_eq!(
            [rect.x, rect.y, rect.width, rect.height],
            [chip_x, (row * 21) as f32, 10., 10.],
            "chip in row {row}"
        );
    }
}

#[test]
fn repeated_float_rows_do_not_spend_the_budget_on_expired_floats() {
    check_rows(&LEFT_ROW.repeat(ROWS), 40.);
}

#[test]
fn older_tall_float_still_excludes_space_among_expired_short_floats() {
    // Clear only the short floats. The older tall float remains active even
    // after several completed blocks of short floats have expired.
    let mut body = String::from("<div class=tall></div>");
    body.push_str(&RIGHT_ROW.repeat(ROWS));
    check_rows(&body, 15.);
}
