use rust_browser_engine::dom::Attributes;
use rust_browser_engine::{Fonts, Layout, LoadedPage, dom::NodeKind};
use std::sync::Arc;

#[test]
fn parser_budget_depth_is_safe_on_one_mib_stack() {
    for (style, calculated) in [
        ("display:block", false),
        ("display:inline", false),
        ("display:flex", false),
        (
            "display:flex;width:calc(100% - 1px);min-height:calc(1px + 0%)",
            true,
        ),
    ] {
        let fonts = Fonts::new().unwrap();
        let mut page = LoadedPage::from_html(
            "https://example.test/",
            "<!doctype html><body style='margin:0'>",
        );
        let body = page.document.elements("body").next().unwrap();
        let doc = Arc::make_mut(&mut page.document);
        let mut parent = body;
        let mut first = None;
        // html + body + 253 divs + anchor = 256 element levels.
        for _ in 0..253 {
            parent = doc.append(
                parent,
                NodeKind::Element {
                    tag: "div".into(),
                    attrs: Attributes::from([("style".into(), style.into())]),
                },
            );
            first.get_or_insert(parent);
        }
        let anchor = doc.append(
            parent,
            NodeKind::Element {
                tag: "a".into(),
                attrs: Attributes::from([("href".into(), "/deep".into())]),
            },
        );
        doc.append(anchor, NodeKind::Text("Deep visible content".into()));
        let layout = std::thread::Builder::new()
            .stack_size(1024 * 1024)
            .spawn(move || Layout::build(&page, &fonts, 800.))
            .unwrap()
            .join()
            .unwrap();
        assert!(layout.limited);
        assert!(layout.links.iter().any(|link| link.url.ends_with("/deep")));
        assert!(layout.height.is_finite());
        assert!(!layout.commands.is_empty());
        if calculated {
            let first = first.unwrap();
            assert_eq!(layout.rect_for(first).unwrap().width, 799.);
            assert_eq!(
                layout.computed_styles[first.0]
                    .min_height
                    .resolve(100., 16.),
                Some(1.)
            );
        }
    }
}

#[test]
fn nested_flex_remeasurement_has_a_work_budget() {
    let fonts = Fonts::new().unwrap();
    let mut source = String::from("<!doctype html><style>body{margin:0}</style>");
    for _ in 0..20 {
        source.push_str("<div style='display:flex'>");
    }
    source.push_str("<a href='/deep'>Deep visible content</a>");
    for level in 0..20 {
        source.push_str(&format!(
            "<div style='height:{}px'></div></div>",
            level * 3 + 30
        ));
    }
    let page = LoadedPage::from_html("https://example.test/", &source);
    let layout = Layout::build(&page, &fonts, 800.);
    assert!(layout.limited);
    assert!(layout.height.is_finite());
    assert!(layout.links.iter().any(|link| link.url.ends_with("/deep")));
}
