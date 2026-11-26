//! Browser commands, distinct from editing, page focus, and OS shortcuts.
use rust_browser_engine::LoadedPage;
use std::fmt::Write;
use winit::keyboard::{Key, ModifiersState, NamedKey};

pub const HELP_URL: &str = "about:shortcuts";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Address,
    NewTab,
    CloseTab,
    ReopenTab,
    DuplicateTab,
    MoveTabLeft,
    MoveTabRight,
    NextTab,
    PreviousTab,
    Reload,
    Back,
    Forward,
    ZoomIn,
    ZoomOut,
    ResetZoom,
    Menu,
    Downloads,
    VisitHistory,
    BookmarkPage,
    ToggleDevtools,
    InspectElement,
}

enum Chord {
    CtrlChar(&'static str),
    CtrlPlainChar(&'static str),
    CtrlShiftChar(&'static str),
    CtrlShiftNamed(NamedKey),
    CtrlTab { shift: bool },
    AltChar(&'static str),
    AltNamed(NamedKey),
    Named(NamedKey),
    PlainNamed(NamedKey),
}

impl Chord {
    fn matches(&self, key: &Key, modifiers: ModifiersState) -> bool {
        fn character(key: &Key, choices: &str) -> bool {
            matches!(key, Key::Character(text) if text.len() == 1
                && choices.as_bytes().contains(&text.as_bytes()[0].to_ascii_lowercase()))
        }

        let ctrl = modifiers.control_key() && !modifiers.alt_key();
        let ctrl_shift = modifiers == ModifiersState::CONTROL | ModifiersState::SHIFT;
        match *self {
            Self::CtrlShiftChar(chars) => ctrl_shift && character(key, chars),
            Self::CtrlShiftNamed(named) => ctrl_shift && *key == Key::Named(named),
            Self::CtrlChar(chars) => ctrl && character(key, chars),
            Self::CtrlPlainChar(chars) => {
                ctrl && !modifiers.shift_key() && !modifiers.super_key() && character(key, chars)
            }
            Self::CtrlTab { shift } => {
                ctrl && modifiers.shift_key() == shift && *key == Key::Named(NamedKey::Tab)
            }
            Self::AltChar(chars) => modifiers.alt_key() && character(key, chars),
            Self::AltNamed(named) => modifiers.alt_key() && *key == Key::Named(named),
            Self::Named(named) => *key == Key::Named(named),
            Self::PlainNamed(named) => modifiers.is_empty() && *key == Key::Named(named),
        }
    }
}

struct Binding {
    command: Command,
    chords: &'static [Chord],
    label: &'static str,
    description: &'static str,
}

const fn binding(
    command: Command,
    chords: &'static [Chord],
    label: &'static str,
    description: &'static str,
) -> Binding {
    Binding {
        command,
        chords,
        label,
        description,
    }
}

const BINDINGS: &[Binding] = &[
    binding(
        Command::Address,
        &[Chord::CtrlChar("l")],
        "Ctrl+L",
        "Focus the address bar",
    ),
    binding(
        Command::NewTab,
        &[Chord::CtrlPlainChar("t")],
        "Ctrl+T",
        "Open a new tab",
    ),
    binding(
        Command::CloseTab,
        &[Chord::CtrlChar("w")],
        "Ctrl+W",
        "Close the active tab",
    ),
    binding(
        Command::ReopenTab,
        &[Chord::CtrlShiftChar("t")],
        "Ctrl+Shift+T",
        "Reopen the most recently closed eligible tab",
    ),
    binding(
        Command::DuplicateTab,
        &[Chord::CtrlShiftChar("k")],
        "Ctrl+Shift+K",
        "Open the current eligible address in a new tab",
    ),
    binding(
        Command::MoveTabLeft,
        &[Chord::CtrlShiftNamed(NamedKey::ArrowLeft)],
        "Ctrl+Shift+Left arrow",
        "Move the active tab one position left while not editing",
    ),
    binding(
        Command::MoveTabRight,
        &[Chord::CtrlShiftNamed(NamedKey::ArrowRight)],
        "Ctrl+Shift+Right arrow",
        "Move the active tab one position right while not editing",
    ),
    binding(
        Command::NextTab,
        &[Chord::CtrlTab { shift: false }],
        "Ctrl+Tab",
        "Switch to the next tab",
    ),
    binding(
        Command::PreviousTab,
        &[Chord::CtrlTab { shift: true }],
        "Ctrl+Shift+Tab",
        "Switch to the previous tab",
    ),
    binding(
        Command::Reload,
        &[Chord::CtrlChar("r"), Chord::Named(NamedKey::F5)],
        "Ctrl+R or F5",
        "Reload the current page",
    ),
    binding(
        Command::Back,
        &[Chord::AltNamed(NamedKey::ArrowLeft)],
        "Alt+Left arrow",
        "Go back in history",
    ),
    binding(
        Command::Forward,
        &[Chord::AltNamed(NamedKey::ArrowRight)],
        "Alt+Right arrow",
        "Go forward in history",
    ),
    binding(
        Command::ZoomIn,
        &[Chord::CtrlChar("+=")],
        "Ctrl++ or Ctrl+=",
        "Zoom in",
    ),
    binding(
        Command::ZoomOut,
        &[Chord::CtrlChar("-")],
        "Ctrl+-",
        "Zoom out",
    ),
    binding(
        Command::ResetZoom,
        &[Chord::CtrlChar("0")],
        "Ctrl+0",
        "Reset zoom to 100%",
    ),
    binding(
        Command::Downloads,
        &[Chord::CtrlPlainChar("j")],
        "Ctrl+J",
        "Toggle the Downloads panel",
    ),
    binding(
        Command::VisitHistory,
        &[Chord::CtrlPlainChar("h")],
        "Ctrl+H",
        "Open browsing history",
    ),
    binding(
        Command::BookmarkPage,
        &[Chord::CtrlPlainChar("d")],
        "Ctrl+D",
        "Bookmark the current page",
    ),
    binding(
        Command::Menu,
        &[Chord::AltChar("f")],
        "Alt+F",
        "Toggle the Satin menu",
    ),
    binding(
        Command::ToggleDevtools,
        &[Chord::PlainNamed(NamedKey::F12), Chord::CtrlShiftChar("i")],
        "F12 / Ctrl+Shift+I",
        "Toggle Developer Tools",
    ),
    binding(
        Command::InspectElement,
        &[Chord::CtrlShiftChar("c")],
        "Ctrl+Shift+C",
        "Pick an element on the page",
    ),
];

/// A repeat of a reserved key is consumed but must not dispatch a command.
pub(crate) fn devtools_toggle(key: &Key, modifiers: ModifiersState, repeat: bool) -> Option<bool> {
    (*key == Key::Named(NamedKey::F12) && command(key, modifiers) == Some(Command::ToggleDevtools))
        .then_some(!repeat)
}

pub fn command(key: &Key, modifiers: ModifiersState) -> Option<Command> {
    BINDINGS
        .iter()
        .find(|binding| {
            binding
                .chords
                .iter()
                .any(|chord| chord.matches(key, modifiers))
        })
        .map(|binding| binding.command)
}

pub fn label(command: Command) -> &'static str {
    BINDINGS
        .iter()
        .find(|binding| binding.command == command)
        .expect("every shortcut command has a binding")
        .label
}

pub fn help_page(url: &url::Url) -> Option<LoadedPage> {
    if url.scheme() != "about" || url.path() != "shortcuts" {
        return None;
    }
    let mut source = String::from(
        r#"<!doctype html><html lang="en"><meta charset="utf-8">
<title>Keyboard shortcuts</title>
<style>
body { margin: 0; background: #1a1a1c; color: #f2f1f1; font: 16px system-ui, sans-serif; }
main { max-width: 680px; margin: 0 auto; padding: 24px; }
p { color: #c7c4c5; }
a { color: #b9caff; }
dl { display: grid; grid-template-columns: 1fr auto; margin: 24px 0; }
dt, dd { margin: 0; padding: 9px 8px; border-bottom: 1px solid #373637; }
kbd { font-family: monospace; }
</style><main>
<h1>Keyboard shortcuts</h1>
<p>Browser commands in Satin for Windows. In the Satin menu, use the arrow keys to
choose an item, Enter to open it, or Esc to dismiss the menu.</p><dl>"#,
    );
    #[cfg(all(windows, target_arch = "x86_64"))]
    source.push_str(
        "<dt>Open or dismiss the installed update panel when its icon is visible</dt><dd><kbd>Alt+U</kbd></dd>",
    );
    for binding in BINDINGS {
        // Both strings are developer-authored literals, never URL or page input.
        let _ = write!(
            source,
            "<dt>{}</dt><dd><kbd>{}</kbd></dd>",
            binding.description, binding.label
        );
    }
    source.push_str(r#"</dl><p><a href="about:home">Start page</a></p></main></html>"#);
    Some(LoadedPage::from_html(url.as_str(), &source))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ModifiersState as Mods;

    fn character(value: &str) -> Key {
        Key::Character(value.into())
    }

    #[test]
    fn maps_existing_browser_commands() {
        let ctrl = Mods::CONTROL;
        let alt = Mods::ALT;
        let cases = [
            (character("L"), ctrl, Command::Address),
            (character("t"), ctrl, Command::NewTab),
            (character("T"), ctrl | Mods::SHIFT, Command::ReopenTab),
            (character("t"), ctrl | Mods::SHIFT, Command::ReopenTab),
            (character("K"), ctrl | Mods::SHIFT, Command::DuplicateTab),
            (
                Key::Named(NamedKey::ArrowLeft),
                ctrl | Mods::SHIFT,
                Command::MoveTabLeft,
            ),
            (
                Key::Named(NamedKey::ArrowRight),
                ctrl | Mods::SHIFT,
                Command::MoveTabRight,
            ),
            (character("w"), ctrl, Command::CloseTab),
            (Key::Named(NamedKey::Tab), ctrl, Command::NextTab),
            (
                Key::Named(NamedKey::Tab),
                ctrl | Mods::SHIFT,
                Command::PreviousTab,
            ),
            (character("r"), ctrl, Command::Reload),
            (character("R"), ctrl | Mods::SHIFT, Command::Reload),
            (Key::Named(NamedKey::F5), Mods::empty(), Command::Reload),
            (Key::Named(NamedKey::F5), ctrl, Command::Reload),
            (Key::Named(NamedKey::ArrowLeft), alt, Command::Back),
            (Key::Named(NamedKey::ArrowRight), alt, Command::Forward),
            (character("+"), ctrl | Mods::SHIFT, Command::ZoomIn),
            (character("="), ctrl, Command::ZoomIn),
            (character("-"), ctrl, Command::ZoomOut),
            (character("0"), ctrl, Command::ResetZoom),
            (character("d"), ctrl, Command::BookmarkPage),
            (character("J"), ctrl, Command::Downloads),
            (character("H"), ctrl, Command::VisitHistory),
            (character("F"), alt, Command::Menu),
        ];
        for (key, modifiers, expected) in cases {
            assert_eq!(
                command(&key, modifiers),
                Some(expected),
                "{modifiers:?} {key:?}"
            );
        }
    }

    #[test]
    fn leaves_missing_and_contextual_commands_unmapped() {
        let ctrl = Mods::CONTROL;
        let cases = [
            // Find and numbered tab selection are not implemented.
            (character("f"), ctrl),
            (Key::Named(NamedKey::F3), Mods::empty()),
            (character("1"), ctrl),
            (character("D"), ctrl | Mods::SHIFT),
            (character("d"), ctrl | Mods::SUPER),
            (character("J"), ctrl | Mods::SHIFT),
            (character("j"), ctrl | Mods::SUPER),
            (character("j"), ctrl | Mods::ALT),
            // Local editing and AltGr text must not become browser commands.
            (character("a"), ctrl),
            (character("l"), ctrl | Mods::ALT),
            (character("t"), Mods::empty()),
            (character("T"), Mods::SHIFT),
            (character("T"), ctrl | Mods::SHIFT | Mods::ALT),
            (character("T"), ctrl | Mods::SHIFT | Mods::SUPER),
            (character("k"), ctrl),
            (Key::Named(NamedKey::ArrowLeft), ctrl),
            (Key::Named(NamedKey::ArrowRight), Mods::SHIFT),
            (Key::Named(NamedKey::Tab), Mods::SHIFT),
        ];
        for (key, modifiers) in cases {
            assert_eq!(command(&key, modifiers), None, "{modifiers:?} {key:?}");
        }
    }

    #[test]
    fn f12_is_reserved_only_without_modifiers_and_does_not_repeat() {
        let f12 = Key::Named(NamedKey::F12);
        assert_eq!(devtools_toggle(&f12, Mods::empty(), false), Some(true));
        assert_eq!(devtools_toggle(&f12, Mods::empty(), true), Some(false));
        for mods in [
            Mods::CONTROL,
            Mods::ALT,
            Mods::SHIFT,
            Mods::SUPER,
            Mods::all(),
        ] {
            assert_eq!(command(&f12, mods), None);
            assert_eq!(devtools_toggle(&f12, mods, false), None);
        }
        assert_eq!(
            devtools_toggle(&Key::Named(NamedKey::F5), Mods::empty(), false),
            None
        );
    }

    #[test]
    fn help_is_static_and_uses_the_binding_inventory() {
        let url = url::Url::parse(HELP_URL).unwrap();
        let page = help_page(&url).unwrap();
        assert_eq!(page.title(), "Keyboard shortcuts");
        assert_eq!(page.stats.script_count, 0);
        let text = page.document.serialize();
        for binding in BINDINGS {
            assert!(text.contains(&format!("<kbd>{}</kbd>", binding.label)));
        }
        let unrelated = url::Url::parse("https://example.com/shortcuts").unwrap();
        assert!(help_page(&unrelated).is_none());
    }
}
