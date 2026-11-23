//! Application preferences and their native chrome panel. No document or script API.
use super::{
    App, CHROME_HOVER, CHROME_LINE, CHROME_MUTED, CHROME_PANEL, CHROME_TEXT, Canvas, Color,
    CursorIcon, Fonts, Key, NamedKey, Rect, Session, rounded_chrome_rect, scaled_rect,
};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
};

const FILE_NAME: &str = "Settings.v1.json";
const MAX_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Startup {
    #[default]
    RestoreTabs,
    StartPage,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Preferences {
    startup: Startup,
}

pub(super) struct Store {
    path: PathBuf,
    current: Preferences,
    unreadable: bool,
}

impl Store {
    // The caller holds the profile lock during settings I/O.
    pub(super) fn open(profile: &Path) -> Self {
        let path = profile.join(FILE_NAME);
        let (current, unreadable) = match read(&path) {
            Ok(preferences) => (preferences, false),
            Err(error) => {
                eprintln!("Satin settings: {error}; keeping the original file");
                (Preferences::default(), true)
            }
        };
        Self {
            path,
            current,
            unreadable,
        }
    }

    pub(super) fn initial_session(&self, saved: Session) -> Session {
        match self.current.startup {
            Startup::RestoreTabs => saved,
            Startup::StartPage => Session::default(),
        }
    }

    fn toggle_restore(&mut self) -> Result<(), String> {
        if self.unreadable {
            return Err("Settings could not be read".into());
        }
        let startup = match self.current.startup {
            Startup::RestoreTabs => Startup::StartPage,
            Startup::StartPage => Startup::RestoreTabs,
        };
        let updated = Preferences { startup };
        let bytes = serde_json::to_vec_pretty(&updated).map_err(|error| error.to_string())?;
        crate::profile::atomic_write(&self.path, &bytes)?;
        self.current = updated;
        Ok(())
    }
}

fn read(path: &Path) -> Result<Preferences, String> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Preferences::default()),
        Err(error) => return Err(error.to_string()),
    };
    let mut bytes = Vec::new();
    file.take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > MAX_BYTES {
        return Err("Settings file exceeds its size limit".into());
    }
    serde_json::from_slice(&bytes).map_err(|error| error.to_string())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Control {
    Startup,
    Done,
}

struct PanelLayout {
    dialog: Rect,
    startup: Rect,
    done: Rect,
}

impl PanelLayout {
    fn new(width: f32, height: f32) -> Self {
        let w = (width - 32.).clamp(0., 500.);
        let x = (width - w) * 0.5;
        let y = ((height - 248.) * 0.5).max(0.);
        Self {
            dialog: scaled_rect(1., x, y, w, 248.),
            startup: scaled_rect(1., x + 24., y + 80., (w - 48.).max(0.), 66.),
            done: scaled_rect(1., x + w - 112., y + 184., 88., 40.),
        }
    }

    fn control(&self, (x, y): (f32, f32), unreadable: bool) -> Option<Control> {
        if self.done.contains(x, y) {
            Some(Control::Done)
        } else if !unreadable && self.startup.contains(x, y) {
            Some(Control::Startup)
        } else {
            None
        }
    }
}

pub(super) struct Panel {
    focus: Control,
    pressed: Option<Control>,
    write_failed: bool,
}

impl Panel {
    pub(super) fn draw(
        &self,
        canvas: &mut Canvas,
        fonts: &mut Fonts,
        scale: f32,
        settings: &Store,
    ) {
        let width = canvas.width as f32 / scale;
        let height = canvas.height as f32 / scale;
        let layout = PanelLayout::new(width, height);
        let Rect { x, y, .. } = layout.dialog;
        canvas.fill_rect(
            scaled_rect(scale, 0., 0., width, height),
            Color(0, 0, 0, 175),
        );
        let mut box_with_border = |rect: Rect, color, border, radius| {
            rounded_chrome_rect(
                canvas,
                scaled_rect(scale, rect.x, rect.y, rect.width, rect.height),
                radius * scale,
                border,
            );
            rounded_chrome_rect(
                canvas,
                scaled_rect(
                    scale,
                    rect.x + 1.,
                    rect.y + 1.,
                    rect.width - 2.,
                    rect.height - 2.,
                ),
                (radius - 1.) * scale,
                color,
            );
        };
        let focus_ring = Color(247, 181, 124, 255);
        box_with_border(layout.dialog, Color(29, 28, 30, 255), CHROME_LINE, 12.);
        box_with_border(
            layout.startup,
            if settings.unreadable {
                CHROME_PANEL
            } else {
                CHROME_HOVER
            },
            if self.focus == Control::Startup {
                focus_ring
            } else {
                CHROME_LINE
            },
            8.,
        );
        box_with_border(
            layout.done,
            CHROME_PANEL,
            if self.focus == Control::Done {
                focus_ring
            } else {
                CHROME_LINE
            },
            8.,
        );

        let check_x = layout.startup.x + 13.;
        let check_y = layout.startup.y + 16.;
        let check = scaled_rect(scale, check_x, check_y, 16., 16.);
        let restored = settings.current.startup == Startup::RestoreTabs;
        rounded_chrome_rect(
            canvas,
            check,
            3. * scale,
            if restored { focus_ring } else { CHROME_MUTED },
        );
        if restored {
            let color = Color(36, 22, 12, 255);
            let (x, y) = (check.x, check.y);
            canvas.stroke_line(
                x + 4. * scale,
                y + 8. * scale,
                x + 7. * scale,
                y + 11. * scale,
                1.8 * scale,
                color,
            );
            canvas.stroke_line(
                x + 7. * scale,
                y + 11. * scale,
                x + 12. * scale,
                y + 5. * scale,
                1.8 * scale,
                color,
            );
        } else {
            rounded_chrome_rect(
                canvas,
                scaled_rect(scale, check_x + 1., check_y + 1., 14., 14.),
                2. * scale,
                CHROME_PANEL,
            );
        }
        let mut label = |text, left, baseline, size, color, bold| {
            canvas.label(
                text,
                left * scale,
                baseline * scale,
                size * scale,
                color,
                bold,
                fonts,
            );
        };
        label("Settings", x + 24., y + 39., 24., CHROME_TEXT, true);
        label("On startup", x + 24., y + 67., 14., CHROME_MUTED, true);
        label(
            "Reopen tabs from your last session",
            check_x + 26.,
            layout.startup.y + 29.,
            15.,
            if settings.unreadable {
                CHROME_MUTED
            } else {
                CHROME_TEXT
            },
            false,
        );
        label(
            "When off, Satin opens the start page instead.",
            check_x + 26.,
            layout.startup.y + 51.,
            13.,
            CHROME_MUTED,
            false,
        );
        let (message, color) = if settings.unreadable {
            (
                "Could not read settings. Using the default; file kept.",
                focus_ring,
            )
        } else if self.write_failed {
            ("Could not save settings. No change was made.", focus_ring)
        } else {
            (
                "Takes effect the next time you open Satin. Default: On.",
                CHROME_MUTED,
            )
        };
        label(message, x + 24., y + 168., 13., color, false);
        label(
            "Done",
            layout.done.x + 27.,
            layout.done.y + 26.,
            15.,
            CHROME_TEXT,
            false,
        );
    }
}

impl App {
    pub(super) fn open_settings(&mut self) {
        self.select_popup = None;
        self.chrome_hover = None;
        self.caption_pressed = None;
        self.last_title_press = None;
        self.hover = None;
        self.preedit = false;
        self.settings_panel = Some(Panel {
            focus: if self.settings.unreadable {
                Control::Done
            } else {
                Control::Startup
            },
            pressed: None,
            write_failed: false,
        });
        if let Some(window) = &self.window {
            window.set_ime_allowed(false);
        }
        self.settings_cursor();
        self.redraw();
    }

    fn close_settings(&mut self) {
        self.settings_panel = None;
        if let Some(window) = &self.window {
            window.set_ime_allowed(true);
            window.set_cursor(CursorIcon::Default);
        }
        self.redraw();
    }

    fn activate_setting(&mut self, control: Control) {
        match control {
            Control::Startup => {
                let result = self.settings.toggle_restore();
                if let Err(error) = &result {
                    eprintln!("Satin settings: {error}");
                }
                if let Some(panel) = &mut self.settings_panel {
                    panel.write_failed = result.is_err();
                }
                self.redraw();
            }
            Control::Done => self.close_settings(),
        }
    }

    pub(super) fn settings_key(&mut self, key: Key) {
        let Some(panel) = &mut self.settings_panel else {
            return;
        };
        if self.modifiers.control_key() || self.modifiers.alt_key() || self.modifiers.super_key() {
            return;
        }
        match key {
            Key::Named(NamedKey::Escape) => self.close_settings(),
            Key::Named(NamedKey::Enter | NamedKey::Space) => {
                let control = panel.focus;
                self.activate_setting(control);
            }
            // With two controls both Tab and Shift+Tab move to the other control.
            Key::Named(NamedKey::Tab) if !self.settings.unreadable => {
                panel.focus = match panel.focus {
                    Control::Startup => Control::Done,
                    Control::Done => Control::Startup,
                };
                self.redraw();
            }
            _ => {}
        }
    }

    fn setting_at_pointer(&self) -> Option<Control> {
        let size = self.window.as_ref()?.inner_size();
        PanelLayout::new(
            size.width as f32 / self.scale,
            size.height as f32 / self.scale,
        )
        .control(self.mouse, self.settings.unreadable)
    }

    pub(super) fn settings_press(&mut self) {
        let control = self.setting_at_pointer();
        if let Some(panel) = &mut self.settings_panel {
            panel.pressed = control;
            if let Some(control) = control {
                panel.focus = control;
                self.redraw();
            }
        }
    }

    pub(super) fn settings_release(&mut self) {
        let pressed = self
            .settings_panel
            .as_mut()
            .and_then(|panel| panel.pressed.take());
        if let Some(control) = pressed
            && self.setting_at_pointer() == Some(control)
        {
            self.activate_setting(control);
        }
    }

    pub(super) fn settings_cursor(&self) {
        if let Some(window) = &self.window {
            let cursor = if self.setting_at_pointer().is_some() {
                CursorIcon::Pointer
            } else {
                CursorIcon::Default
            };
            window.set_cursor(cursor);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::Profile;
    use std::{fs, time::SystemTime};

    fn disposable_profile() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/settings-tests")
            .join(format!("{}-{nonce}", std::process::id()))
    }

    #[test]
    fn preferences_survive_profile_reopen_without_rewriting_the_saved_session() {
        let path = disposable_profile();
        let (profile, _) = Profile::open(path.clone()).unwrap();
        profile
            .save(&Session {
                schema: 1,
                tabs: vec!["about:media".into(), "about:version".into()],
                active: 1,
            })
            .unwrap();
        let original = fs::read(path.join("session.json")).unwrap();
        let mut settings = Store::open(&path);
        assert_eq!(settings.current.startup, Startup::RestoreTabs);
        assert!(!path.join(FILE_NAME).exists());
        settings.toggle_restore().unwrap();
        assert_eq!(fs::read(path.join("session.json")).unwrap(), original);
        drop(profile);

        let (profile, saved) = Profile::open(path.clone()).unwrap();
        let mut settings = Store::open(&path);
        let start = settings.initial_session(saved);
        assert_eq!(start.tabs, ["about:home"]);
        assert_eq!(start.active, 0);
        settings.toggle_restore().unwrap();
        drop(profile);

        let (profile, saved) = Profile::open(path.clone()).unwrap();
        let restored = Store::open(&path).initial_session(saved);
        assert_eq!(restored.tabs, ["about:media", "about:version"]);
        assert_eq!(restored.active, 1);
        drop(profile);
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn unreadable_preferences_fall_back_without_overwriting_original_bytes() {
        let path = disposable_profile();
        let (profile, _) = Profile::open(path.clone()).unwrap();
        let file = path.join(FILE_NAME);
        for original in [
            br#"{"startup":"future_mode"}"#.to_vec(),
            br#"{"startup":"start_page","future":true}"#.to_vec(),
            vec![b' '; MAX_BYTES + 1],
        ] {
            fs::write(&file, &original).unwrap();
            let mut settings = Store::open(&path);
            assert!(settings.unreadable);
            assert_eq!(settings.current.startup, Startup::RestoreTabs);
            assert!(settings.toggle_restore().is_err());
            assert_eq!(fs::read(&file).unwrap(), original);
        }
        drop(profile);
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn a_failed_save_does_not_apply_the_new_value() {
        let path = disposable_profile();
        let (profile, _) = Profile::open(path.clone()).unwrap();
        let mut settings = Store::open(&path);
        settings.toggle_restore().unwrap();
        let bytes = fs::read(path.join(FILE_NAME)).unwrap();
        fs::create_dir(path.join(FILE_NAME).with_extension("pending")).unwrap();
        assert!(settings.toggle_restore().is_err());
        assert_eq!(settings.current.startup, Startup::StartPage);
        assert_eq!(fs::read(path.join(FILE_NAME)).unwrap(), bytes);
        drop(profile);
        fs::remove_dir_all(path).unwrap();
    }
}
