use rust_browser_engine::{
    DrawCommand, Fonts, Layout, LoadedPage,
    css::Color,
    dom::NodeId,
    history::ScriptNavigation,
    layout::Viewport,
    loader::{HOME_FOCUS_ADDRESS_URL, OpenedPage, builtin},
    script::{DocumentSession, FormKey, ScriptRuntime},
};

fn viewport(width: f32, height: f32) -> Viewport {
    Viewport {
        width,
        height,
        ..Viewport::default()
    }
}

fn open_home(viewport: Viewport) -> (LoadedPage, ScriptRuntime) {
    let opened = OpenedPage::internal(&"about:home".parse().unwrap(), Default::default());
    let (mut page, mut session) = DocumentSession::start(opened).unwrap();
    let mut runtime = None;
    for _ in 0..64 {
        if session.done() {
            break;
        }
        session.advance(&mut page, &mut runtime, viewport).unwrap();
        assert!(session.next_resource().is_none(), "home must load offline");
    }
    assert!(session.done());
    let runtime = runtime.unwrap();
    assert!(runtime.diagnostics().is_empty());
    assert!(runtime.take_navigation_request().is_none());
    (page, runtime)
}

fn search_button(page: &LoadedPage) -> NodeId {
    page.document
        .elements("button")
        .find(|&id| page.document.attr(id, "id") == Some("home-search"))
        .unwrap()
}

#[test]
fn built_in_home_has_only_the_polish_wordmark_and_address_activation() {
    let (page, _) = open_home(viewport(1280., 708.));
    let doc = &page.document;
    assert_eq!(page.title(), "Nowa karta");
    assert_eq!(
        doc.attr(doc.elements("html").next().unwrap(), "lang"),
        Some("pl")
    );
    assert_eq!(doc.elements("main").count(), 1);
    assert_eq!(doc.text(doc.elements("h1").next().unwrap()).trim(), "Satin");
    let button = search_button(&page);
    assert_eq!(doc.elements("button").count(), 1);
    assert_eq!(doc.attr(button, "type"), Some("button"));
    assert_eq!(doc.attr(button, "aria-keyshortcuts"), Some("Control+L"));
    assert!(doc.text(button).contains("Wyszukaj lub wpisz adres"));
    for tag in ["a", "input", "form", "nav", "img", "link"] {
        assert!(doc.elements(tag).next().is_none(), "unexpected {tag}");
    }
    assert!(page.images.is_empty());
    let scripts: Vec<_> = doc.elements("script").collect();
    assert_eq!(scripts.len(), 1);
    assert_eq!(doc.attr(scripts[0], "src"), None);
    assert_eq!(builtin("home").title(), page.title());
}

#[test]
fn mouse_enter_and_space_request_the_same_native_address_action() {
    let view = viewport(1280., 708.);
    let (page, mut runtime) = open_home(view);
    let button = search_button(&page);
    let fonts = Fonts::new().unwrap();
    let layout = Layout::build_with_viewport(&page, &fonts, view);
    let rect = layout.rect_for(button).unwrap();
    let click_target = layout
        .element_at(rect.x + rect.width / 2., rect.y + rect.height / 2.)
        .unwrap();
    assert_eq!(runtime.click_with_default(click_target), (true, None));
    assert_address_request(&runtime);

    assert!(runtime.form_keydown(FormKey::Tab, false, false).unwrap());
    assert_eq!(runtime.focused_control(), Some(button));
    for key in [FormKey::Enter, FormKey::Space] {
        assert!(runtime.form_keydown(key, false, false).unwrap());
        assert_address_request(&runtime);
    }
    assert!(runtime.take_form_submissions().is_empty());
    assert!(runtime.diagnostics().is_empty());
}

fn assert_address_request(runtime: &ScriptRuntime) {
    assert!(matches!(
        runtime.take_navigation_request(),
        Some(ScriptNavigation::Navigate { url, .. }) if url.as_str() == HOME_FOCUS_ADDRESS_URL
    ));
    assert!(runtime.take_navigation_request().is_none());
}

#[test]
fn engine_layout_centers_the_quiet_surface_at_desktop_and_narrow_widths() {
    let page = builtin("home");
    let fonts = Fonts::new().unwrap();
    let title = page.document.elements("h1").next().unwrap();
    let button = search_button(&page);
    let hint = page.document.elements("kbd").next().unwrap();
    for (width, height, expected_width, center) in [
        (1586., 900., 486., 0.47),
        (1280., 708., 486., 0.47),
        (320., 400., 284., 0.5),
        (240., 216., 204., 0.5),
    ] {
        let layout = Layout::build_with_viewport(&page, &fonts, viewport(width, height));
        assert_eq!(layout.background, Color(25, 25, 28, 255));
        assert!(!layout.limited);
        assert!(layout.content_width <= width + 1.);

        let title = layout.rect_for(title).unwrap();
        let search = layout.rect_for(button).unwrap();
        assert!((search.width - expected_width).abs() < 1., "{search:?}");
        assert!((search.x + search.width / 2. - width / 2.).abs() < 1.);
        assert!(search.height >= 47.);
        assert!(search.y > title.y + title.height);
        assert!(((title.y + search.y + search.height) / 2. - height * center).abs() < 3.);
        assert_eq!(layout.rect_for(hint).is_some(), width > 300.);
        assert_eq!(
            layout
                .commands
                .iter()
                .filter(|command| matches!(command, DrawCommand::Svg { .. }))
                .count(),
            2
        );
    }
}
