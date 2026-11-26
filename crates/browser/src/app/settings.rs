//! Application preferences and their native chrome panel. No document or script API.
use super::{App, CursorIcon, Key, NamedKey, Session};
use chrome::ChromeState;
use panel::Control;
pub(super) use panel::Panel;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::{self, Read},
    path::{Path, PathBuf},
};

pub(super) mod chrome;
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

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Preferences {
    startup: Startup,
    #[serde(default = "default_bookmarks_bar")]
    bookmarks_bar: bool,
    #[serde(default)]
    quiet_mode: bool,
}

const fn default_bookmarks_bar() -> bool {
    true
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            startup: Startup::default(),
            bookmarks_bar: default_bookmarks_bar(),
            quiet_mode: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Access {
    ReadWrite,
    ReadOnly,
    Unreadable,
}

pub(super) struct Store {
    path: PathBuf,
    current: Preferences,
    access: Access,
}

impl Store {
    // The caller holds the profile lock during settings I/O.
    pub(super) fn open(profile: &Path) -> Self {
        let path = profile.join(FILE_NAME);
        let (current, access) = match read(&path) {
            Ok(state) => state,
            Err(error) => {
                eprintln!("Satin settings: {error}; keeping the original file");
                (Preferences::default(), Access::Unreadable)
            }
        };
        Self {
            path,
            current,
            access,
        }
    }

    pub(super) fn initial_session(&self, saved: Session) -> Session {
        match self.current.startup {
            Startup::RestoreTabs => saved,
            Startup::StartPage => Session::default(),
        }
    }

    fn can_update(&self) -> bool {
        self.access == Access::ReadWrite
    }

    pub(super) fn chrome(&self) -> ChromeState {
        ChromeState::new(
            self.current.bookmarks_bar,
            self.current.quiet_mode,
            self.can_update(),
        )
    }

    fn update(&mut self, edit: impl FnOnce(&mut Preferences)) -> Result<(), String> {
        match self.access {
            Access::ReadWrite => {}
            Access::ReadOnly => return Err("Settings file is read-only".into()),
            Access::Unreadable => return Err("Settings could not be read".into()),
        }
        // Recheck an explicit read-only attribute that may have been set since
        // opening. On some platforms renaming over a read-only file can succeed.
        match fs::metadata(&self.path) {
            Ok(meta) if meta.permissions().readonly() => {
                self.access = Access::ReadOnly;
                return Err("Settings file is read-only".into());
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
        let mut updated = self.current;
        edit(&mut updated);
        let bytes = serde_json::to_vec_pretty(&updated).map_err(|error| error.to_string())?;
        crate::profile::atomic_write(&self.path, &bytes)?;
        self.current = updated;
        Ok(())
    }

    fn toggle_restore(&mut self) -> Result<(), String> {
        self.update(|prefs| {
            prefs.startup = match prefs.startup {
                Startup::RestoreTabs => Startup::StartPage,
                Startup::StartPage => Startup::RestoreTabs,
            };
        })
    }

    fn toggle_bookmarks_bar(&mut self) -> Result<(), String> {
        self.update(|prefs| prefs.bookmarks_bar = !prefs.bookmarks_bar)
    }

    fn toggle_quiet(&mut self) -> Result<(), String> {
        self.update(|prefs| prefs.quiet_mode = !prefs.quiet_mode)
    }
}

fn read(path: &Path) -> Result<(Preferences, Access), String> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok((Preferences::default(), Access::ReadWrite));
        }
        Err(error) => return Err(error.to_string()),
    };
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    let access = if metadata.permissions().readonly() {
        Access::ReadOnly
    } else {
        Access::ReadWrite
    };
    let mut bytes = Vec::new();
    file.take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > MAX_BYTES {
        return Err("Settings file exceeds its size limit".into());
    }
    let preferences = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    Ok((preferences, access))
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
        self.settings_panel = Some(Panel::new(self.settings.can_update()));
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
            Control::RestoreTabs | Control::BookmarksBar => {
                let result = match control {
                    Control::RestoreTabs => self.settings.toggle_restore(),
                    _ => self.settings.toggle_bookmarks_bar(),
                };
                if let Err(error) = &result {
                    eprintln!("Satin settings: {error}");
                }
                if let Some(panel) = &mut self.settings_panel {
                    panel.set_write_failed(result.is_err());
                }
                if !self.settings.can_update()
                    && let Some(panel) = &mut self.settings_panel
                {
                    panel.focus_category();
                    self.settings_cursor();
                }
                if result.is_ok() && control == Control::BookmarksBar {
                    self.chrome_preferences_changed();
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
                panel.step_focus(self.modifiers.shift_key(), self.settings.can_update());
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
            self.window_regions(self.active).page.y,
            self.mouse,
            self.settings.can_update(),
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

    /// Both native entry points (toolbar and menu) dispatch here.
    pub(super) fn toggle_quiet_mode(&mut self) {
        const SAVE_ERROR: &str = "Nie można zapisać trybu skupienia. Nie wprowadzono zmian.";
        match self.settings.toggle_quiet() {
            Ok(()) => {
                if self.status == SAVE_ERROR {
                    self.status.clear();
                }
                self.chrome_preferences_changed();
            }
            Err(error) => {
                eprintln!("Satin settings: {error}");
                self.status = SAVE_ERROR.into();
                self.redraw();
            }
        }
    }

    fn chrome_preferences_changed(&mut self) {
        // Do not route clicks against pre-change toolbar/row bounds while the
        // next frame is queued. This does not touch bookmarks or downloads data.
        self.hits.clear();
        self.chrome_hover = None;
        self.dismiss_address_suggestions();
        self.dismiss_downloads();
        #[cfg(all(windows, target_arch = "x86_64"))]
        self.dismiss_update_panel();
        self.window_geometry_changed();
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
    fn startup_only_file_migrates_on_edit_and_each_preference_preserves_the_others() {
        let path = disposable_profile();
        let (profile, _) = Profile::open(path.clone()).unwrap();
        let file = path.join(FILE_NAME);
        let original = br#"{"startup":"start_page"}"#;
        fs::write(&file, original).unwrap();
        let mut settings = Store::open(&path);
        assert_eq!(settings.current.startup, Startup::StartPage);
        assert!(settings.chrome().bookmarks_bar_visible());
        assert!(!settings.current.quiet_mode);
        assert_eq!(fs::read(&file).unwrap(), original);

        settings.toggle_quiet().unwrap();
        assert!(settings.current.bookmarks_bar);
        assert!(!settings.chrome().bookmarks_bar_visible());
        settings.toggle_bookmarks_bar().unwrap();
        assert!(settings.current.quiet_mode);
        assert_eq!(settings.current.startup, Startup::StartPage);
        settings.toggle_quiet().unwrap();
        assert!(!settings.current.bookmarks_bar);
        assert!(!settings.chrome().bookmarks_bar_visible());
        settings.toggle_quiet().unwrap();
        settings.toggle_restore().unwrap();
        drop(profile);

        let (profile, _) = Profile::open(path.clone()).unwrap();
        let settings = Store::open(&path);
        assert_eq!(settings.current.startup, Startup::RestoreTabs);
        assert!(!settings.current.bookmarks_bar);
        assert!(settings.current.quiet_mode);
        assert!(!path.join("session.json").exists());
        drop(profile);
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn read_only_file_preserves_preferences_and_rejects_all_writes() {
        let path = disposable_profile();
        let (profile, _) = Profile::open(path.clone()).unwrap();
        let file = path.join(FILE_NAME);
        let original = br#"{"startup":"start_page","bookmarks_bar":false,"quiet_mode":true}"#;
        fs::write(&file, original).unwrap();
        let mut before_permissions_change = Store::open(&path);
        let permissions = fs::metadata(&file).unwrap().permissions();
        let mut read_only = permissions.clone();
        read_only.set_readonly(true);
        fs::set_permissions(&file, read_only).unwrap();
        let mut settings = Store::open(&path);
        assert!(before_permissions_change.toggle_quiet().is_err());
        assert_eq!(before_permissions_change.access, Access::ReadOnly);
        assert_eq!(settings.access, Access::ReadOnly);
        assert_eq!(settings.current.startup, Startup::StartPage);
        assert!(!settings.current.bookmarks_bar);
        assert!(settings.current.quiet_mode);
        assert!(settings.toggle_quiet().is_err());
        assert!(settings.toggle_bookmarks_bar().is_err());
        assert!(settings.toggle_restore().is_err());
        assert!(!file.with_extension("pending").exists());
        assert_eq!(fs::read(&file).unwrap(), original);
        fs::set_permissions(&file, permissions).unwrap();
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
            br#"{"startup":"start_page","bookmarks_bar":"off"}"#.to_vec(),
            br#"{"startup":"start_page","quiet_mode":null}"#.to_vec(),
            vec![b' '; MAX_BYTES + 1],
        ] {
            fs::write(&file, &original).unwrap();
            let mut settings = Store::open(&path);
            assert_eq!(settings.access, Access::Unreadable);
            assert_eq!(settings.current.startup, Startup::RestoreTabs);
            assert!(settings.current.bookmarks_bar);
            assert!(!settings.current.quiet_mode);
            assert!(settings.toggle_restore().is_err());
            assert!(settings.toggle_bookmarks_bar().is_err());
            assert!(settings.toggle_quiet().is_err());
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
        settings.toggle_bookmarks_bar().unwrap();
        settings.toggle_quiet().unwrap();
        let bytes = fs::read(path.join(FILE_NAME)).unwrap();
        fs::create_dir(path.join(FILE_NAME).with_extension("pending")).unwrap();
        assert!(settings.toggle_restore().is_err());
        assert!(settings.toggle_bookmarks_bar().is_err());
        assert!(settings.toggle_quiet().is_err());
        assert_eq!(settings.current.startup, Startup::StartPage);
        assert!(!settings.current.bookmarks_bar);
        assert!(settings.current.quiet_mode);
        assert_eq!(fs::read(path.join(FILE_NAME)).unwrap(), bytes);
        drop(profile);
        fs::remove_dir_all(path).unwrap();
    }
}
