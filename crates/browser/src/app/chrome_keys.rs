use std::collections::HashSet;
use winit::{event::ElementState, keyboard::PhysicalKey};

/// A key consumed by a flyout must not repeat into another surface after
/// dismissal, including when releasing Alt changes the repeated logical key.
#[derive(Default)]
pub(super) struct OwnedKeys {
    held: HashSet<PhysicalKey>,
    captured: HashSet<PhysicalKey>,
}

impl OwnedKeys {
    pub(super) fn capture_held(&mut self) {
        self.captured.extend(self.held.iter().copied());
    }

    pub(super) fn blocks(
        &mut self,
        physical: PhysicalKey,
        state: ElementState,
        repeat: bool,
        synthetic: bool,
        owns_key: bool,
    ) -> bool {
        if state == ElementState::Released {
            if !synthetic {
                self.held.remove(&physical);
                self.captured.remove(&physical);
            }
            return true;
        }
        if synthetic {
            return owns_key || self.captured.contains(&physical);
        }
        self.held.insert(physical);
        if repeat {
            if owns_key {
                self.captured.insert(physical);
            }
            return self.captured.contains(&physical);
        }
        // A fresh physical press is new intent, even if a release went to
        // another window while this window was unfocused.
        self.captured.remove(&physical);
        if owns_key {
            self.captured.insert(physical);
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::KeyCode;

    #[test]
    fn an_existing_held_key_cannot_repeat_after_a_flyout_closes() {
        use ElementState::{Pressed, Released};
        let key = PhysicalKey::Code(KeyCode::Enter);
        let mut keys = OwnedKeys::default();
        assert!(!keys.blocks(key, Pressed, false, false, false));
        keys.capture_held();
        assert!(keys.blocks(key, Pressed, true, false, false));
        assert!(keys.blocks(key, Released, false, true, false));
        assert!(keys.blocks(key, Pressed, true, false, false));
        assert!(keys.blocks(key, Released, false, false, false));
        assert!(!keys.blocks(key, Pressed, false, false, false));
    }
}
