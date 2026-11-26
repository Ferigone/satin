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
    let heading = doc.elements("h1").next().unwrap();
    assert_eq!(doc.text(heading).trim(), "Satin");
    assert_eq!(doc.attr(heading, "aria-label"), Some("Satin"));
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
    for (width, height, expected_width) in [
        (1586., 900., 486.),
        (1280., 679., 486.),
        (320., 400., 284.),
        (240., 216., 204.),
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
        let center = if width > 660. {
            height * 0.47 - 3.5
        } else {
            height * 0.5
        };
        assert!(((title.y + search.y + search.height) / 2. - center).abs() < 1.);
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
fn quiet_home_tracking_scales_with_the_wordmark_and_keeps_a_proportional_keycap() {
    let page = builtin("home");
    let fonts = Fonts::new().unwrap();
    let doc = &page.document;
    let mark = doc.elements("svg").next().unwrap();
    let word = doc
        .elements("span")
        .find(|&node| doc.attr(node, "class") == Some("home-brand-text"))
        .unwrap();
    let letters = &doc.nodes[word.0].children;
    assert_eq!(letters.len(), 5);
    let hint = doc.elements("kbd").next().unwrap();

    for (width, height) in [(1280., 679.), (540., 500.)] {
        let layout = Layout::build_with_viewport(&page, &fonts, viewport(width, height));
        let mark = layout.rect_for(mark).unwrap();
        let word = layout.rect_for(word).unwrap();
        let letters: Vec<_> = letters
            .iter()
            .map(|&letter| layout.rect_for(letter).unwrap())
            .collect();
        let font_size = mark.width / 0.54;
        let tracking = font_size * 0.085;
        for pair in letters.windows(2) {
            let gap = pair[1].x - (pair[0].x + pair[0].width);
            assert!((gap + tracking).abs() < 0.3, "{gap} != -{tracking}");
        }
        let full_width: f32 = letters.iter().map(|letter| letter.width).sum();
        assert!((word.width - (full_width - 5. * tracking)).abs() < 0.3);
        assert!(((mark.x + word.x + word.width) / 2. - width / 2.).abs() < 0.5);
        let center_delta = (word.y + word.height / 2.) - (mark.y + mark.height / 2.);
        assert!((center_delta + font_size * 0.02).abs() < 0.3);
        let hint = layout.rect_for(hint).unwrap();
        assert!((30. ..=40.).contains(&hint.width), "{hint:?}");
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
