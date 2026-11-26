use std::ops::Range;

#[path = "address_bar/chrome.rs"]
pub(super) mod chrome;
#[path = "address_bar/suggestions.rs"]
pub(super) mod suggestions;

const MAX_ADDRESS_BYTES: usize = 16_384;

pub(crate) enum Movement {
    Left,
    Right,
    Start,
    End,
}

/// Both ends of the selection are UTF-8 character boundaries. The caret is
/// the moving end, so Shift can shrink or reverse a selection.
#[derive(Default)]
pub(crate) struct AddressBar {
    text: String,
    anchor: usize,
    caret: usize,
}

impl AddressBar {
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    pub(crate) fn set_text(&mut self, text: String) {
        self.text = text;
        self.collapse(self.text.len());
    }

    pub(crate) fn caret(&self) -> usize {
        self.caret
    }

    pub(crate) fn selection(&self) -> Range<usize> {
        self.anchor.min(self.caret)..self.anchor.max(self.caret)
    }

    pub(crate) fn selected_text(&self) -> Option<&str> {
        let range = self.selection();
        (!range.is_empty()).then(|| &self.text[range])
    }

    pub(crate) fn select_all(&mut self) {
        self.anchor = 0;
        self.caret = self.text.len();
    }

    pub(crate) fn move_caret(&mut self, movement: Movement, extend: bool) {
        let selected = self.selection();
        self.caret = match movement {
            Movement::Left if !extend && !selected.is_empty() => selected.start,
            Movement::Right if !extend && !selected.is_empty() => selected.end,
            Movement::Left => self.previous(),
            Movement::Right => self.next(),
            Movement::Start => 0,
            Movement::End => self.text.len(),
        };
        if !extend {
            self.anchor = self.caret;
        }
    }

    pub(crate) fn insert(&mut self, text: &str) -> bool {
        let filtered: String = text.chars().filter(|c| !c.is_control()).collect();
        self.insert_text(&filtered)
    }

    /// Insert unchanged text, including line boundaries in a developer command.
    pub(crate) fn insert_text(&mut self, text: &str) -> bool {
        let selected = self.selection();
        if text.is_empty()
            || text.len()
                > MAX_ADDRESS_BYTES - (self.text.len() - selected.len()).min(MAX_ADDRESS_BYTES)
        {
            return false;
        }
        self.replace(selected, text);
        true
    }

    pub(crate) fn delete(&mut self, backward: bool) {
        let mut range = self.selection();
        if range.is_empty() {
            range = if backward {
                self.previous()..self.caret
            } else {
                self.caret..self.next()
            };
        }
        self.replace(range, "");
    }

    pub(crate) fn delete_selection(&mut self) {
        self.replace(self.selection(), "");
    }

    fn previous(&self) -> usize {
        self.text[..self.caret]
            .char_indices()
            .next_back()
            .map_or(0, |(index, _)| index)
    }

    fn next(&self) -> usize {
        self.caret
            + self.text[self.caret..]
                .chars()
                .next()
                .map_or(0, char::len_utf8)
    }

    fn collapse(&mut self, at: usize) {
        self.caret = at;
        self.anchor = at;
    }

    fn replace(&mut self, range: Range<usize>, text: &str) {
        let at = range.start + text.len();
        self.text.replace_range(range, text);
        self.collapse(at);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address(text: &str) -> AddressBar {
        let mut address = AddressBar::default();
        address.set_text(text.into());
        address
    }

    #[test]
    fn shift_reverses_and_replaces_only_selected_unicode_text() {
        let mut bar = address("/ż🦊尾.");
        bar.move_caret(Movement::Left, false);
        bar.move_caret(Movement::Left, true);
        bar.move_caret(Movement::Left, true);
        assert_eq!(bar.selected_text(), Some("🦊尾"));
        bar.move_caret(Movement::Right, true);
        assert_eq!(bar.selected_text(), Some("尾"));
        bar.move_caret(Movement::Right, true);
        bar.move_caret(Movement::Right, true);
        assert_eq!(bar.selected_text(), Some("."));
        assert!(bar.insert("ś"));
        assert_eq!(bar.text(), "/ż🦊尾ś");
        assert_eq!(bar.caret(), bar.text().len());
        assert_eq!(bar.selected_text(), None);
    }

    #[test]
    fn unmodified_arrows_collapse_to_the_corresponding_edge() {
        let mut bar = address("abcż");
        bar.move_caret(Movement::Left, true);
        bar.move_caret(Movement::Left, false);
        assert_eq!(bar.caret(), 3);
        assert_eq!(bar.selected_text(), None);
        bar.move_caret(Movement::Start, true);
        assert_eq!(bar.selected_text(), Some("abc"));
        bar.move_caret(Movement::Right, false);
        assert_eq!(bar.caret(), 3);
        assert_eq!(bar.selected_text(), None);
        bar.move_caret(Movement::End, true);
        assert_eq!(bar.selected_text(), Some("ż"));
        bar.select_all();
        assert_eq!(bar.selected_text(), Some("abcż"));
    }

    #[test]
    fn backspace_delete_and_cut_remove_only_the_selection() {
        for remove in [
            |bar: &mut AddressBar| bar.delete(true),
            |bar: &mut AddressBar| bar.delete(false),
            AddressBar::delete_selection,
        ] {
            let mut bar = address("a🦊z");
            bar.move_caret(Movement::Left, false);
            bar.move_caret(Movement::Left, true);
            remove(&mut bar);
            assert_eq!(bar.text(), "az");
            assert_eq!(bar.caret(), 1);
            assert_eq!(bar.selected_text(), None);
        }
    }

    #[test]
    fn rejected_input_does_not_destroy_the_selection() {
        let mut bar = address("abc");
        bar.select_all();
        assert!(!bar.insert("\r\n"));
        assert!(!bar.insert(&"x".repeat(MAX_ADDRESS_BYTES + 1)));
        assert_eq!(bar.selected_text(), Some("abc"));
        assert!(bar.insert(&"x".repeat(MAX_ADDRESS_BYTES)));
        assert_eq!(bar.text().len(), MAX_ADDRESS_BYTES);
        assert!(!bar.insert("a"));
    }
}
