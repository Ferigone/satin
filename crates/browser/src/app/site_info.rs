//! Read-only browser chrome. Neither the editor draft nor a pending/failed
//! navigation can supply an origin or connection status to this surface.

use super::{
    Action, App, CHROME_HOVER, CHROME_ICON, CHROME_LINE, CHROME_MUTED, CHROME_PANEL, CHROME_TEXT,
    Canvas, Color, CursorIcon, Key, NamedKey, Rect, Tab, TabId, chrome_box, chrome_elide,
    chrome_shape, icons::Glyph, paint_centered_glyph, physical_rect, quiet_cinder, resize_cursor,
    scaled_rect,
};
use url::Url;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Pending,
    Failed,
    Internal,
    File,
    Http,
    Https,
    Other,
}

impl Kind {
    fn from_document(url: Option<&str>, failed: bool) -> Self {
        if failed {
            return Self::Failed;
        }
        let Some(url) = url else {
            return Self::Pending;
        };
        match url.split_once(':').map(|(scheme, _)| scheme) {
            Some("about") => Self::Internal,
            Some("file") => Self::File,
            Some("http") => Self::Http,
            Some("https") => Self::Https,
            _ => Self::Other,
        }
    }

    fn glyph(self) -> Glyph {
        match self {
            Self::Https => Glyph::Lock,
            Self::Internal => Glyph::Compass,
            Self::File => Glyph::FileText,
            _ => Glyph::Search,
        }
    }

    fn copy(self) -> (&'static str, Option<&'static str>) {
        match self {
            Self::Internal => ("Wewnętrzna strona Satin.", None),
            Self::File => ("Dokument otwarto z adresu file:.", None),
            Self::Http => (
                "HTTP · adres dokumentu",
                Some("HTTP nie szyfruje połączenia."),
            ),
            Self::Https => (
                "HTTPS · adres dokumentu",
                Some("Satin nie pokazuje szczegółów certyfikatu."),
            ),
            Self::Other => ("Adres dokumentu spoza HTTP(S).", None),
            Self::Pending => ("Brak otwartego dokumentu.", None),
            Self::Failed => ("Nie udało się otworzyć strony.", None),
        }
    }
}

// Display only canonical authority, not a title supplied by the web page,
// URL credentials, a local path, a query, or a fragment.
fn web_title(url: &str) -> Option<String> {
    let url = Url::parse(url).ok()?;
    let host = url.host_str()?;
    Some(if let Some(port) = url.port() {
        format!("{host}:{port}")
    } else {
        host.to_owned()
    })
}

fn tab_document(tab: &Tab) -> (Option<&str>, bool) {
    (
        tab.inspected_page().map(|page| page.url.as_str()),
        tab.recovery.is_error_page(),
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Binding {
    tab: TabId,
    navigation: u64,
    document: u64,
    kind: Kind,
}

impl Binding {
    fn new(tab: &Tab, kind: Kind) -> Self {
        Self {
            tab: tab.id,
            navigation: tab.navigation,
            document: tab.document_navigation,
            kind,
        }
    }
}

pub(super) struct Panel {
    binding: Binding,
    host: Option<String>,
}

impl Panel {
    fn new(tab: &Tab) -> Self {
        let (url, failed) = tab_document(tab);
        let kind = Kind::from_document(url, failed);
        Self {
            binding: Binding::new(tab, kind),
            host: matches!(kind, Kind::Http | Kind::Https)
                .then(|| url.and_then(web_title))
                .flatten(),
        }
    }

    fn current(&self, tab: &Tab) -> bool {
        let (url, failed) = tab_document(tab);
        self.binding == Binding::new(tab, Kind::from_document(url, failed))
    }
}

struct Layout {
    panel: Rect,
    close: Rect,
}

impl Layout {
    fn new(anchor: Rect, width: f32, height: f32, details: bool) -> Option<Self> {
        const MARGIN: f32 = 8.;
        let w = (width - 18.).min(352.);
        let h = if details { 108. } else { 72. };
        if w < 70. || height < h + 2. * MARGIN {
            return None;
        }
        let x = (anchor.x + anchor.width - w).clamp(MARGIN, width - w - MARGIN);
        let below = anchor.y + anchor.height + 5.;
        let above = anchor.y - h - 6.;
        let bottom = height - h - MARGIN;
        let y = if below > bottom && above >= MARGIN {
            above
        } else {
            below.clamp(MARGIN, bottom)
        };
        Some(Self {
            panel: scaled_rect(1., x, y, w, h),
            close: scaled_rect(1., x + w - 40., y + 10., 26., 26.),
        })
    }
}

// Reuse the existing location slot. At unsupported extreme widths, never
// register an invisible target over the editor, bookmark, or updater.
fn anchor(toolbar: &quiet_cinder::Toolbar) -> Option<Rect> {
    (toolbar.location.right() <= toolbar.bookmark.x).then(|| chrome_box(toolbar.location))
}

fn press_dismisses(presented: bool, left: bool, target: Option<Action>) -> bool {
    presented
        && match target {
            Some(Action::SiteInfoSurface) => false,
            Some(Action::SiteInfoClose | Action::SiteInfo) => left,
            _ => true,
        }
}

impl App {
    pub(super) fn draw_site_anchor(
        &mut self,
        canvas: &mut Canvas,
        toolbar: &quiet_cinder::Toolbar,
    ) {
        let Some(bounds) = anchor(toolbar) else {
            return;
        };
        let (url, failed) = tab_document(&self.tabs[self.active]);
        let hot = self.chrome_hover == Some(Action::SiteInfo) || self.site_info.is_some();
        if hot {
            canvas.fill_rounded_rect(
                physical_rect(self.scale, bounds),
                [6. * self.scale; 4],
                CHROME_HOVER,
            );
        }
        paint_centered_glyph(
            canvas,
            bounds,
            17.,
            self.scale,
            Kind::from_document(url, failed).glyph(),
            if hot { CHROME_TEXT } else { CHROME_ICON },
        );
        self.hits.push((bounds, Action::SiteInfo));
    }

    pub(super) fn toggle_site_info(&mut self) {
        if self.site_info.is_some() {
            self.dismiss_site_info();
            return;
        }
        if self.preedit
            || self.settings_panel.is_some()
            || self.history_menu.is_some()
            || self.downloads_panel.is_some()
            || self.update_panel_open()
        {
            return;
        }
        self.site_info = Some(Panel::new(&self.tabs[self.active]));
        self.chrome_keys.capture_held();
        self.tab_hits_current = false;
        self.dismiss_address_suggestions();
        self.select_popup = None;
        self.chrome_menu = None;
        self.caption_pressed = None;
        self.last_title_press = None;
        self.wheel = None;
        self.resizing_dock = false;
        self.hover = None;
        self.redraw();
    }

    pub(super) fn dismiss_site_info(&mut self) {
        if self.site_info.take().is_some() {
            self.tab_hits_current = false;
            self.chrome_hover = None;
            if let Some(window) = &self.window {
                window.set_cursor(CursorIcon::Default);
            }
            self.redraw();
        }
    }

    pub(super) fn draw_site_info(&mut self, canvas: &mut Canvas, width: f32, height: f32) {
        let Some(open) = &self.site_info else {
            return;
        };
        if !open.current(&self.tabs[self.active]) {
            self.dismiss_site_info();
            return;
        }
        let (summary, note) = open.binding.kind.copy();
        let Some(layout) = anchor(&self.toolbar_layout(width))
            .and_then(|anchor| Layout::new(anchor, width, height, note.is_some()))
        else {
            self.dismiss_site_info();
            return;
        };
        let u = self.scale;
        let bounds = physical_rect(u, layout.panel);
        canvas.fill_rounded_rect(
            Rect {
                y: bounds.y + 4. * u,
                ..bounds
            },
            [10. * u; 4],
            Color(0, 0, 0, 72),
        );
        canvas.fill_rounded_rect(bounds, [10. * u; 4], CHROME_PANEL);
        canvas.outline_rounded_rect(bounds, [10. * u; 4], -u, u, CHROME_LINE);
        self.hits.push((layout.panel, Action::SiteInfoSurface));
        self.hits.push((layout.close, Action::SiteInfoClose));

        let x = layout.panel.x + 14.;
        let y = layout.panel.y;
        let title = open.host.as_deref().unwrap_or("Informacje o stronie");
        let title_width = layout.close.x - x - 6.;
        let title = chrome_elide(&self.fonts, title, title_width * u, 13. * u, 700);
        let title = chrome_shape(&self.fonts, &title, 13. * u, 700);
        let close_hot = self.chrome_hover == Some(Action::SiteInfoClose);
        if close_hot {
            canvas.fill_rounded_rect(physical_rect(u, layout.close), [6. * u; 4], CHROME_HOVER);
        }
        paint_centered_glyph(canvas, layout.close, 17., u, Glyph::X, CHROME_ICON);
        canvas.clip(scaled_rect(u, x, y, title_width, 37.));
        canvas.text(x * u, (y + 28.) * u, &title, CHROME_TEXT, &mut self.fonts);
        canvas.clip(bounds);
        let mut label = |canvas: &mut Canvas, text: &str, baseline: f32| {
            let text = chrome_elide(
                &self.fonts,
                text,
                (layout.panel.width - 28.) * u,
                12. * u,
                400,
            );
            let run = chrome_shape(&self.fonts, &text, 12. * u, 400);
            canvas.text(
                x * u,
                (y + baseline) * u,
                &run,
                CHROME_MUTED,
                &mut self.fonts,
            );
        };
        label(canvas, summary, 51.);
        if let Some(note) = note {
            canvas.fill_rect(
                scaled_rect(u, x, y + 63., layout.panel.width - 28., 1.),
                CHROME_LINE,
            );
            label(canvas, note, 86.);
        }
        canvas.clip(scaled_rect(
            1.,
            0.,
            0.,
            canvas.width as f32,
            canvas.height as f32,
        ));
    }

    // Stay modal while a resize is awaiting presentation. No stale hit may
    // dismiss the panel or activate a control behind it.
    pub(super) fn site_info_press(&mut self, left: bool) {
        if press_dismisses(self.tab_hits_current, left, self.chrome_action_at_pointer()) {
            self.dismiss_site_info();
        }
    }

    pub(super) fn update_site_info_pointer(&mut self) {
        let action = self.chrome_action_at_pointer().filter(|action| {
            matches!(action, Action::SiteInfo | Action::SiteInfoClose) || action.caption()
        });
        if action != self.chrome_hover {
            self.chrome_hover = action;
            self.redraw();
        }
        if let Some(window) = &self.window {
            window.set_cursor(self.frame_resize_at_pointer().map_or_else(
                || match action {
                    Some(Action::SiteInfo | Action::SiteInfoClose) => CursorIcon::Pointer,
                    _ => CursorIcon::Default,
                },
                resize_cursor,
            ));
        }
    }

    pub(super) fn site_info_key(&mut self, key: &Key, repeat: bool, browser_command: bool) -> bool {
        if self.site_info.is_none() {
            return false;
        }
        if !repeat {
            if matches!(key, Key::Named(NamedKey::Escape))
                || (self.modifiers.is_empty()
                    && matches!(key, Key::Named(NamedKey::Enter | NamedKey::Space)))
            {
                self.dismiss_site_info();
            } else if browser_command {
                // A fresh explicit browser shortcut may switch surface or tab.
                self.dismiss_site_info();
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_only_the_committed_document() {
        for (url, expected) in [
            ("about:home", Kind::Internal),
            ("file://server/private.jpg", Kind::File),
            ("http://localhost:8000/", Kind::Http),
            ("https://example.test/", Kind::Https),
            ("data:text/plain,private", Kind::Other),
        ] {
            assert_eq!(Kind::from_document(Some(url), false), expected);
            assert_eq!(Kind::from_document(Some(url), true), Kind::Failed);
        }
        assert_eq!(Kind::from_document(None, false), Kind::Pending);
    }

    #[test]
    fn host_is_credential_free_and_includes_only_a_non_default_port() {
        assert_eq!(
            web_title("https://user:password@example.test:443/path?q=secret#private"),
            Some("example.test".into())
        );
        assert_eq!(
            web_title("http://[::1]:8080/path"),
            Some("[::1]:8080".into())
        );
    }

    #[test]
    fn popup_and_close_fit_narrow_and_quiet_toolbars_even_with_an_update() {
        for width in [320., 390., 660., 661., 1280.] {
            for update in [false, true] {
                for quiet in [false, true] {
                    let toolbar = quiet_cinder::Toolbar::new(width, update, quiet);
                    let anchor = anchor(&toolbar).unwrap();
                    assert!(anchor.x >= toolbar.address.x);
                    assert!(anchor.x + anchor.width <= toolbar.bookmark.x);
                    assert!(anchor.x + anchor.width < toolbar.edit.x);
                    let layout = Layout::new(anchor, width, 400., true).unwrap();
                    assert!(layout.panel.x >= 8.);
                    assert!(layout.panel.x + layout.panel.width <= width - 8.);
                    assert_eq!(layout.panel.y, anchor.y + anchor.height + 5.);
                    assert!(
                        layout
                            .panel
                            .contains(layout.close.x + 13., layout.close.y + 13.)
                    );
                }
            }
        }
    }

    #[test]
    fn close_and_outside_presses_wait_for_a_current_frame() {
        for target in [None, Some(Action::SiteInfoClose), Some(Action::Address)] {
            assert!(!press_dismisses(false, true, target));
            assert!(press_dismisses(true, true, target));
        }
        assert!(press_dismisses(true, true, Some(Action::SiteInfo)));
        assert!(!press_dismisses(true, false, Some(Action::SiteInfoClose)));
        assert!(!press_dismisses(true, true, Some(Action::SiteInfoSurface)));
    }
}
