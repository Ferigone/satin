use rust_browser_engine::{
    DrawCommand, Fonts, Layout, LoadedPage, css::Color, loader::SvgImage, paint::Canvas,
    script::ScriptRuntime,
};
use std::sync::Arc;

fn id(page: &LoadedPage, name: &str) -> rust_browser_engine::dom::NodeId {
    page.document
        .connected_nodes()
        .find(|&id| page.document.attr(id, "id") == Some(name))
        .unwrap()
}
#[test]
fn svg_dom_rects_ignore_stroke_and_clip_but_actual_hits_respect_them() {
    let mut page = LoadedPage::from_html(
        "https://example.test/",
        r##"<!doctype html><style>body{margin:0}svg{display:block;--paint:#008000}circle{fill:var(--paint);stroke:blue;stroke-width:8}</style><svg id="root" width="100" height="100"><defs><clipPath id="clip"><rect x="40" width="20" height="100"/></clipPath></defs><a href="/linked"><circle id="circle" cx="50" cy="50" r="20" clip-path="url(#clip)"/></a></svg>"##,
    );
    let fonts = Fonts::new().unwrap();
    let layout = Layout::build(&page, &fonts, 100.);
    let circle = id(&page, "circle");
    let root = id(&page, "root");
    let rect = layout.rect_for(circle).unwrap();
    assert!((rect.width - 40.).abs() < 0.05);
    assert!((rect.height - 40.).abs() < 0.05);
    assert_eq!(layout.element_at(50., 50.), Some(circle));
    assert_eq!(layout.element_at(35., 50.), Some(root));
    assert_eq!(
        layout.link_at(50., 50.),
        Some("https://example.test/linked")
    );
    assert_eq!(layout.link_at(35., 50.), None);
    let mut canvas = Canvas::new(100, 100, Color::WHITE);
    let mut fonts = fonts;
    canvas.document(&layout, &mut fonts, 0., 0.);
    assert_eq!(canvas.pixels[50 * 100 + 50], 0x008000);
    assert_eq!(canvas.pixels[50 * 100 + 35], 0xffffff);
    assert!(!canvas.limited);
    assert!(!layout.limited);
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    assert_eq!(runtime.click_link(circle).as_deref(), Some("/linked"));
    runtime
        .eval("document.getElementById('circle').setAttribute('fill','red')")
        .unwrap();
    runtime.snapshot(&mut page);
    // A presentation attribute still loses to the stylesheet declaration.
    let next = Layout::build(&page, &fonts, 100.);
    canvas.clear(Color::WHITE);
    canvas.document(&next, &mut fonts, 0., 0.);
    assert_eq!(canvas.pixels[50 * 100 + 50], 0x008000);
}
#[test]
fn svg_image_is_inert_preserves_viewbox_and_reuses_scene_geometry() {
    let asset=Arc::new(SvgImage::from_source(r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100" viewBox="0 0 100 100"><style>rect{fill:#008000}</style><rect width="100" height="100"/><script>throw 'never execute'</script></svg>"##).unwrap());
    let mut page = LoadedPage::from_html(
        "https://example.test/",
        "<!doctype html><style>body{margin:0}img{display:block}</style><img id=one src='/art.svg' width=200 height=80><img id=two src='/art.svg' width=200 height=80>",
    );
    page.svg_images
        .insert("https://example.test/art.svg".into(), asset);
    assert!(ScriptRuntime::for_page(&mut page).unwrap().is_none());
    let mut fonts = Fonts::new().unwrap();
    let layout = Layout::build(&page, &fonts, 200.);
    let scenes = layout
        .commands
        .iter()
        .filter_map(|c| match c {
            DrawCommand::Svg {
                scene,
                interactive: false,
                ..
            } => Some(scene),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(scenes.len(), 2);
    assert!(Arc::ptr_eq(scenes[0], scenes[1]));
    let mut canvas = Canvas::new(200, 160, Color::WHITE);
    canvas.document(&layout, &mut fonts, 0., 0.);
    assert_eq!(canvas.pixels[40 * 200 + 70], 0x008000);
    assert_eq!(canvas.pixels[40 * 200 + 10], 0xffffff);
    assert_eq!(layout.element_at(80., 40.), Some(id(&page, "one")));
    assert_eq!(layout.rect_for(id(&page, "one")).unwrap().width, 200.);
}
#[test]
fn xml_metadata_and_svg_links_keep_their_namespace() {
    let page=LoadedPage::from_svg("https://example.test/art.svg",r##"<?xml version="1.0"?><svg xmlns="http://www.w3.org/2000/svg" xmlns:links="http://www.w3.org/1999/xlink" width="100" height="100"><metadata><meta xmlns="http://www.w3.org/1999/xhtml" name="purpose" content="fixture"/></metadata><a links:href="/target"><rect id="shape" width="100" height="100" fill="green"/></a></svg>"##).unwrap();
    assert!(page.document.html_elements("body").next().is_none());
    assert_eq!(
        page.document.namespace(id(&page, "shape")),
        rust_browser_engine::dom::Namespace::Svg
    );
    let fonts = Fonts::new().unwrap();
    let layout = Layout::build(&page, &fonts, 100.);
    assert_eq!(
        layout.link_at(50., 50.),
        Some("https://example.test/target")
    );
    assert!(
        LoadedPage::from_svg(
            "https://example.test/art.svg",
            "<!DOCTYPE svg SYSTEM 'https://example.test/evil'><svg/>"
        )
        .is_err()
    );
}
#[test]
fn svg_scene_and_hit_geometry_share_css_affine_and_overflow_clips() {
    let page = LoadedPage::from_html(
        "https://example.test/",
        r##"<!doctype html><style>body{margin:0}#container{position:absolute;left:30px;top:30px;width:40px;height:40px;overflow:hidden;transform:rotate(45deg);transform-origin:0 0}svg{display:block}</style><div id="container"><svg width="80" height="80"><rect id="shape" width="80" height="80" fill="green"/></svg></div>"##,
    );
    let mut fonts = Fonts::new().unwrap();
    let layout = Layout::build(&page, &fonts, 100.);
    let shape = id(&page, "shape");
    assert_eq!(layout.element_at(30., 50.), Some(shape));
    assert_ne!(layout.element_at(5., 32.), Some(shape));
    let mut canvas = Canvas::new(100, 100, Color::WHITE);
    canvas.document(&layout, &mut fonts, 0., 0.);
    assert_eq!(canvas.pixels[50 * 100 + 30], 0x008000);
    assert_eq!(canvas.pixels[32 * 100 + 5], 0xffffff);
    assert!(!canvas.limited);
}
