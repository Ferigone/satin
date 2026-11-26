use rust_browser_engine::{
    DrawCommand, Fonts, Layout, LoadedPage,
    css::Color,
    dom::NodeId,
    history::{ScriptNavigation, SessionHistory},
    layout::Viewport,
    loader::{HOME_FOCUS_ADDRESS_URL, OpenedPage, START_SHORTCUTS_URL, builtin},
    script::{DocumentSession, FormKey, ScriptRuntime},
    web_storage::{StorageProfile, TabStorage},
};
use url::Url;

fn viewport(width: f32, height: f32) -> Viewport {
    Viewport {
        width,
        height,
        ..Viewport::default()
    }
}

fn open_builtin(url: &str, viewport: Viewport, storage: TabStorage) -> (LoadedPage, ScriptRuntime) {
    let url = Url::parse(url).unwrap();
    let opened = OpenedPage::internal(&url, Default::default());
    let (mut page, mut session) =
        DocumentSession::start_in_tab(opened, SessionHistory::new(url), storage).unwrap();
    let mut runtime = None;
    for _ in 0..64 {
        if session.done() {
            break;
        }
        session.advance(&mut page, &mut runtime, viewport).unwrap();
        assert!(
            session.next_resource().is_none(),
            "built-in start pages must load offline"
        );
    }
    assert!(session.done());
    let runtime = runtime.unwrap();
    assert!(runtime.diagnostics().is_empty());
    assert!(runtime.take_navigation_request().is_none());
    (page, runtime)
}

fn open_home(viewport: Viewport) -> (LoadedPage, ScriptRuntime) {
    open_builtin("about:home", viewport, TabStorage::default())
}

fn node_by_id(page: &LoadedPage, id: &str) -> NodeId {
    page.document
        .connected_nodes()
        .find(|&node| page.document.attr(node, "id") == Some(id))
        .unwrap()
}

fn search_button(page: &LoadedPage) -> NodeId {
    let node = node_by_id(page, "home-search");
    assert_eq!(page.document.tag(node), "button");
    node
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

#[test]
fn alternate_start_route_keeps_custom_shortcuts_and_demos_off_the_quiet_home() {
    let view = viewport(1280., 708.);
    let profile = StorageProfile::default();
    let storage = TabStorage::new(profile.clone());
    let (_, mut old_home) = open_builtin("about:home", view, storage.clone());
    old_home
        .eval(
            r#"localStorage.setItem('satin.home.shortcuts', '[{"name":"Mój skrót","url":"https://saved.test/"}]');"#,
        )
        .unwrap();
    let (quiet, _) = open_builtin("about:home", view, storage.clone());
    assert!(quiet.document.elements("a").next().is_none());
    assert!(quiet.document.elements("nav").next().is_none());
    assert!(
        !quiet
            .document
            .text(quiet.document.root)
            .contains("Mój skrót")
    );

    let (page, mut shortcuts) = open_builtin(START_SHORTCUTS_URL, view, storage);
    let (_, mut other_tab) = open_builtin(START_SHORTCUTS_URL, view, TabStorage::new(profile));
    assert_eq!(page.url, START_SHORTCUTS_URL);
    assert_eq!(page.title(), "Start shortcuts");
    assert!(page.images.contains_key("about:satin-mark"));
    assert!(page.images.contains_key("about:satin-add-icon"));
    let saved = node_by_id(&page, "home-custom-0");
    assert_eq!(page.document.text(saved), "Mój skrót");
    assert_eq!(
        page.document.attr(saved, "href"),
        Some("https://saved.test/")
    );

    let demos = node_by_id(&page, "home-demos-toggle");
    assert!(shortcuts.click(demos));
    assert_eq!(
        shortcuts
            .eval("!document.getElementById('home-demos').hasAttribute('hidden')")
            .unwrap()
            .as_bool(),
        Some(true)
    );
    assert_eq!(
        shortcuts
            .click_link(node_by_id(&page, "home-video"))
            .as_deref(),
        Some("about:media")
    );

    assert!(shortcuts.click(node_by_id(&page, "home-add-toggle")));
    shortcuts
        .eval(
            "document.getElementById('home-shortcut-name').value = 'Added'; \
             document.getElementById('home-shortcut-url').value = 'https://added.test/';",
        )
        .unwrap();
    assert!(shortcuts.click(node_by_id(&page, "home-shortcut-save")));
    assert_eq!(
        old_home
            .eval(
                "JSON.parse(localStorage.getItem('satin.home.shortcuts'))[1].url === 'https://added.test/'"
            )
            .unwrap()
            .as_bool(),
        Some(true)
    );
    other_tab.pump();
    for runtime in [&mut shortcuts, &mut other_tab] {
        assert_eq!(
            runtime
                .eval("document.getElementById('home-custom-1').textContent === 'Added'")
                .unwrap()
                .as_bool(),
            Some(true)
        );
        assert!(runtime.diagnostics().is_empty());
    }
}
