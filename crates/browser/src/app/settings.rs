//! Application preferences and their native chrome panel. No document or script API.
use super::{App, CursorIcon, Key, NamedKey, Session};
use panel::Control;
pub(super) use panel::Panel;
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
};

mod panel;

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

impl App {
    pub(super) fn open_settings(&mut self) {
        self.dismiss_history_menu();
        self.chrome_menu = None;
        self.select_popup = None;
        self.chrome_hover = None;
        self.caption_pressed = None;
        self.last_title_press = None;
        self.hover = None;
        self.preedit = false;
        self.wheel = None;
        self.resizing_dock = false;
        self.settings_panel = Some(Panel::new(self.settings.unreadable));
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
        if self.pointer_inside {
            self.update_pointer(false);
        }
        self.redraw();
    }

    fn activate_setting(&mut self, control: Control) {
        match control {
            Control::Section(section) => {
                if let Some(panel) = &mut self.settings_panel {
                    panel.select(section);
                }
                self.settings_cursor();
                self.redraw();
            }
            Control::RestoreTabs => {
                let result = self.settings.toggle_restore();
                if let Err(error) = &result {
                    eprintln!("Satin settings: {error}");
                }
                if let Some(panel) = &mut self.settings_panel {
                    panel.set_write_failed(result.is_err());
                }
                self.redraw();
            }
            Control::Version | Control::Shortcuts => {
                self.close_settings();
                let target = if control == Control::Version {
                    "about:version"
                } else {
                    crate::shortcuts::HELP_URL
                };
                self.navigate(target.into(), true);
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
                let control = panel.focused_control();
                self.activate_setting(control);
            }
            Key::Named(NamedKey::Tab) => {
                panel.step_focus(self.modifiers.shift_key(), self.settings.unreadable);
                self.redraw();
            }
            _ => {}
        }
    }

    fn setting_at_pointer(&self) -> Option<Control> {
        if !self.pointer_inside {
            return None;
        }
        let size = self.window.as_ref()?.inner_size();
        self.settings_panel.as_ref()?.control_at(
            size.width as f32 / self.scale,
            size.height as f32 / self.scale,
            self.mouse,
            self.settings.unreadable,
        )
    }

    pub(super) fn settings_press(&mut self) {
        let control = self.setting_at_pointer();
        if let Some(panel) = &mut self.settings_panel
            && panel.press(control)
        {
            self.redraw();
        }
    }

    pub(super) fn settings_release(&mut self) {
        let hovered = self.setting_at_pointer();
        if let Some(panel) = &mut self.settings_panel
            && let Some(control) = panel.release()
        {
            if Some(control) == hovered {
                self.activate_setting(control);
            } else {
                self.redraw();
            }
        }
    }

    pub(super) fn settings_cursor(&mut self) {
        if !self.pointer_inside {
            self.settings_cancel_pointer();
        }
        let control = self.setting_at_pointer();
        if let Some(window) = &self.window {
            let cursor = if control.is_some() {
                CursorIcon::Pointer
            } else {
                CursorIcon::Default
            };
            window.set_cursor(cursor);
        }
        if let Some(panel) = &mut self.settings_panel
            && panel.set_hover(control)
        {
            self.redraw();
        }
    }

    pub(super) fn settings_cancel_pointer(&mut self) {
        if let Some(panel) = &mut self.settings_panel
            && panel.cancel_pointer()
        {
            self.redraw();
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
