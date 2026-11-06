mod key;
mod shortcut;

use std::time::{Duration, Instant};

pub use key::{Key, Modifier, Side};
pub use shortcut::{Shortcut, check_pair};

use shortcut::Held;

const CAPTURE_LIMIT: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TalkKey {
    Hold,
    Toggle,
}

impl TalkKey {
    pub fn from_name(name: &str) -> Option<TalkKey> {
        match name {
            "hold" => Some(TalkKey::Hold),
            "toggle" => Some(TalkKey::Toggle),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Event {
    Pressed,
    Released,
    Cancelled,
}

pub struct TalkKeys {
    held: bool,
    active: bool,
    hold: Shortcut,
    toggle: Shortcut,
    modifiers: Held,
    capturing: Option<TalkKey>,
    capture_started: Instant,
    toggle_held: bool,
    toggle_active: bool,
    suppress_release: bool,
    altgr_adds_ctrl: bool,
}

impl TalkKeys {
    pub fn new(hold: Shortcut, toggle: Shortcut, altgr_adds_ctrl: bool) -> Self {
        Self {
            held: false,
            active: false,
            hold,
            toggle,
            modifiers: Held::default(),
            capturing: None,
            capture_started: Instant::now(),
            toggle_held: false,
            toggle_active: false,
            suppress_release: false,
            altgr_adds_ctrl,
        }
    }

    pub fn handle(&mut self, key: Key, down: bool, up: bool) -> (bool, Option<Event>) {
        if down || up {
            self.modifiers.set(key, down);
        }

        if self.capturing.is_some() && self.capture_started.elapsed() > CAPTURE_LIMIT {
            self.capturing = None;
        }

        if self.capturing.is_some() {
            return (false, None);
        }

        if key == Key::Escape && down && self.active {
            self.suppress_release = self.held;
            self.held = false;
            self.active = false;
            self.toggle_active = false;
            return (true, Some(Event::Cancelled));
        }

        if self.held && !self.hold.modifiers_held(&self.modifiers) {
            self.held = false;
            self.suppress_release = true;
            return (false, Some(Event::Released));
        }

        if self.hold.key_matches(key) && self.suppress_release {
            if up {
                self.suppress_release = false;
            }

            return (true, None);
        }

        if self.hold.key_matches(key) && up && self.held {
            self.held = false;
            return (true, Some(Event::Released));
        }

        if self.toggle.key_matches(key) && self.toggle_held {
            if up {
                self.toggle_held = false;
            }

            return (true, None);
        }

        if down
            && self
                .hold
                .matches(key, &self.modifiers, self.altgr_adds_ctrl)
        {
            if !self.active {
                self.held = true;
                self.active = true;
                return (true, Some(Event::Pressed));
            }

            return (true, None);
        }

        if down
            && self
                .toggle
                .matches(key, &self.modifiers, self.altgr_adds_ctrl)
        {
            self.toggle_held = true;
            if !self.active {
                self.active = true;
                self.toggle_active = true;
                return (true, Some(Event::Pressed));
            }

            if self.toggle_active {
                self.toggle_active = false;
                return (true, Some(Event::Released));
            }

            return (true, None);
        }

        (false, None)
    }

    pub fn involves(&self, key: Key) -> bool {
        key == Key::Escape
            || key.modifier().is_some()
            || self.hold.key_matches(key)
            || self.toggle.key_matches(key)
    }

    pub fn active(&self) -> bool {
        self.active
    }

    pub fn capturing(&self) -> bool {
        self.capturing.is_some()
    }

    pub fn busy(&self) -> bool {
        self.active || self.capturing.is_some()
    }

    /// Whether the active recording was started with the toggle key (it keeps
    /// running until stopped) rather than the hold key.
    pub fn toggle_active(&self) -> bool {
        self.active && self.toggle_active
    }

    pub fn begin_capture(&mut self, which: TalkKey) -> Result<(), String> {
        if self.busy() {
            return Err("Cannot change talk keys while recording or capturing".to_owned());
        }

        self.capturing = Some(which);
        self.capture_started = Instant::now();
        Ok(())
    }

    pub fn cancel_capture(&mut self) {
        self.capturing = None;
    }

    pub fn complete_capture(&mut self, which: TalkKey) -> Result<(), String> {
        let current = self.capturing.ok_or("Talk-key capture is not active")?;
        if current != which {
            return Err("Wrong talk key is being captured".to_owned());
        }

        self.capturing = None;
        Ok(())
    }

    pub fn set_shortcuts(&mut self, hold: Shortcut, toggle: Shortcut) {
        self.hold = hold;
        self.toggle = toggle;
        self.modifiers = Held::default();
    }

    pub fn finish(&mut self) {
        self.active = false;
        self.held = false;
        self.toggle_active = false;
    }

    /// Ends the active recording as if the talk key had been released: used
    /// by the overlay's Stop button and when the recording reaches its length
    /// limit. A hold key that is still down is ignored until it is let go.
    pub fn request_stop(&mut self) -> bool {
        if !self.active || !(self.held || self.toggle_active) {
            return false;
        }

        if self.held {
            self.held = false;
            self.suppress_release = true;
        }
        self.toggle_active = false;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const F8: Key = Key::Function(8);
    const F9: Key = Key::Function(9);
    const F12: Key = Key::Function(12);
    const SPACE: Key = Key::Space;
    const ESCAPE: Key = Key::Escape;
    const DOWN: Key = Key::Down;
    const L: Key = Key::Letter('L');
    const CTRL: Key = Key::Modifier(Modifier::Ctrl, Side::Left);
    const RIGHT_CTRL: Key = Key::Modifier(Modifier::Ctrl, Side::Right);
    const SHIFT: Key = Key::Modifier(Modifier::Shift, Side::Left);
    const RIGHT_ALT: Key = Key::Modifier(Modifier::Alt, Side::Right);
    const RIGHT_META: Key = Key::Modifier(Modifier::Meta, Side::Right);

    fn shortcut(text: &str) -> Shortcut {
        Shortcut::parse(text).unwrap()
    }

    fn keys(hold: &str, toggle: &str) -> TalkKeys {
        TalkKeys::new(shortcut(hold), shortcut(toggle), true)
    }

    #[test]
    fn request_stop_ends_hold_or_toggle_once_and_swallows_the_hold_key_up() {
        let mut state = keys("F8", "Ctrl+Shift+Space");
        assert!(!state.request_stop());
        assert_eq!(state.handle(F8, true, false), (true, Some(Event::Pressed)));
        assert!(state.request_stop());
        assert!(!state.request_stop());
        assert_eq!(state.handle(F8, false, true), (true, None));

        state.active = false;
        for key in [CTRL, SHIFT] {
            state.handle(key, true, false);
        }
        assert_eq!(
            state.handle(SPACE, true, false),
            (true, Some(Event::Pressed))
        );
        assert!(state.toggle_active);
        assert!(state.request_stop());
        assert!(!state.toggle_active);
    }

    #[test]
    fn hold_release_cancel_and_busy_press_are_distinct() {
        let mut state = keys("F8", "Ctrl+Shift+Space");
        assert_eq!(state.handle(F8, true, false), (true, Some(Event::Pressed)));
        assert_eq!(state.handle(F8, true, false), (true, None));
        assert_eq!(state.handle(F8, false, true), (true, Some(Event::Released)));
        assert_eq!(state.handle(F8, true, false), (true, None));
        assert_eq!(
            state.handle(ESCAPE, true, false),
            (true, Some(Event::Cancelled))
        );
        state.hold = shortcut("F9");
        assert_eq!(state.handle(F8, true, false), (false, None));
        assert_eq!(state.handle(F9, true, false), (true, Some(Event::Pressed)));
    }

    #[test]
    fn toggle_ignores_repeats_and_releases_then_stops_on_second_press() {
        let mut state = keys("F8", "Ctrl+Shift+Space");
        assert_eq!(state.handle(CTRL, true, false), (false, None));
        assert_eq!(state.handle(SHIFT, true, false), (false, None));
        assert_eq!(
            state.handle(SPACE, true, false),
            (true, Some(Event::Pressed))
        );
        assert_eq!(state.handle(SPACE, true, false), (true, None));
        assert_eq!(state.handle(SPACE, false, true), (true, None));
        assert_eq!(
            state.handle(SPACE, true, false),
            (true, Some(Event::Released))
        );
        assert_eq!(state.handle(SPACE, false, true), (true, None));
    }

    #[test]
    fn capture_swallows_nothing_and_never_activates() {
        let mut state = keys("F8", "Ctrl+Shift+Space");
        state.begin_capture(TalkKey::Hold).unwrap();
        assert_eq!(state.handle(RIGHT_META, true, false), (false, None));
        assert_eq!(state.handle(RIGHT_CTRL, true, false), (false, None));
        assert_eq!(state.handle(F12, true, false), (false, None));
        assert!(!state.active);
        assert!(shortcut("Right Ctrl+Right Meta+F12").matches(F12, &state.modifiers, true));
        assert!(state.begin_capture(TalkKey::Toggle).is_err());
        assert!(state.complete_capture(TalkKey::Toggle).is_err());
        state.complete_capture(TalkKey::Hold).unwrap();
        assert!(!state.capturing());
    }

    #[test]
    fn right_alt_hold_accepts_windows_synthetic_ctrl() {
        let mut state = keys("Right Alt", "Ctrl+Shift+Space");
        assert_eq!(state.handle(CTRL, true, false), (false, None));
        assert_eq!(
            state.handle(RIGHT_ALT, true, false),
            (true, Some(Event::Pressed))
        );
        assert_eq!(
            state.handle(RIGHT_ALT, false, true),
            (true, Some(Event::Released))
        );
        assert_eq!(state.handle(CTRL, false, true), (false, None));
    }

    #[test]
    fn right_alt_hold_needs_ctrl_up_without_the_altgr_rule() {
        let mut state = TalkKeys::new(shortcut("Right Alt"), shortcut("F9"), false);
        assert_eq!(state.handle(CTRL, true, false), (false, None));
        assert_eq!(state.handle(RIGHT_ALT, true, false), (false, None));
    }

    #[test]
    fn right_alt_alone_toggles_with_or_without_windows_synthetic_ctrl() {
        for synthetic_ctrl in [false, true] {
            let mut state = keys("Ctrl+Space", "Right Alt");
            if synthetic_ctrl {
                assert_eq!(state.handle(CTRL, true, false), (false, None));
            }

            assert_eq!(
                state.handle(RIGHT_ALT, true, false),
                (true, Some(Event::Pressed))
            );
            assert_eq!(state.handle(RIGHT_ALT, false, true), (true, None));
            assert_eq!(
                state.handle(RIGHT_ALT, true, false),
                (true, Some(Event::Released))
            );
        }
    }

    #[test]
    fn right_alt_toggle_accepts_windows_synthetic_ctrl() {
        let mut state = keys("Ctrl+Space", "Right Alt+Down");
        assert_eq!(state.handle(CTRL, true, false), (false, None));
        assert_eq!(state.handle(RIGHT_ALT, true, false), (false, None));
        assert_eq!(
            state.handle(DOWN, true, false),
            (true, Some(Event::Pressed))
        );
        assert_eq!(state.handle(DOWN, false, true), (true, None));
        assert_eq!(state.handle(RIGHT_ALT, false, true), (false, None));
        assert_eq!(state.handle(CTRL, false, true), (false, None));
        assert_eq!(state.handle(CTRL, true, false), (false, None));
        assert_eq!(state.handle(RIGHT_ALT, true, false), (false, None));
        assert_eq!(
            state.handle(DOWN, true, false),
            (true, Some(Event::Released))
        );
    }

    #[test]
    fn sequential_capture_does_not_activate_or_choose_first_key() {
        let mut state = keys("F8", "Ctrl+Shift+Space");
        state.begin_capture(TalkKey::Hold).unwrap();
        assert_eq!(state.handle(CTRL, true, false), (false, None));
        assert_eq!(state.handle(CTRL, false, true), (false, None));
        assert_eq!(state.handle(L, true, false), (false, None));
        assert_eq!(state.handle(L, false, true), (false, None));
        assert_eq!(state.capturing, Some(TalkKey::Hold));
        assert!(!state.active);
        state.cancel_capture();
        state.set_shortcuts(shortcut("Ctrl+L"), shortcut("Ctrl+Shift+Space"));
        assert_eq!(state.handle(CTRL, true, false), (false, None));
        assert_eq!(state.handle(L, true, false), (true, Some(Event::Pressed)));
    }

    #[test]
    fn hold_releases_when_modifier_released_and_suppresses_space_key_up() {
        let mut state = keys("Ctrl+Space", "Ctrl+Shift+Space");
        assert_eq!(state.handle(CTRL, true, false), (false, None));
        assert_eq!(
            state.handle(SPACE, true, false),
            (true, Some(Event::Pressed))
        );
        assert_eq!(
            state.handle(CTRL, false, true),
            (false, Some(Event::Released))
        );
        assert_eq!(state.handle(SPACE, true, false), (true, None));
        assert_eq!(state.handle(SPACE, false, true), (true, None));
    }

    #[test]
    fn involves_only_talk_keys_modifiers_and_escape() {
        let state = keys("F8", "Ctrl+Shift+Space");
        for key in [F8, SPACE, ESCAPE, CTRL, RIGHT_ALT] {
            assert!(state.involves(key), "{key:?}");
        }
        assert!(!state.involves(L));
        assert!(!state.involves(F9));
    }
}
