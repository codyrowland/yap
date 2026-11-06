use super::key::{Key, Modifier, Side};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Need {
    None,
    Either,
    Right,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct Held([bool; 8]);

impl Held {
    pub(super) fn set(&mut self, key: Key, down: bool) {
        if let Key::Modifier(modifier, side) = key {
            self.0[modifier.index() * 2 + usize::from(side == Side::Right)] = down;
        }
    }

    fn left(&self, modifier: Modifier) -> bool {
        self.0[modifier.index() * 2]
    }

    fn right(&self, modifier: Modifier) -> bool {
        self.0[modifier.index() * 2 + 1]
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Shortcut {
    key: Key,
    modifiers: [Need; 4],
}

impl Shortcut {
    pub fn parse(text: &str) -> Result<Shortcut, String> {
        let mut tokens = text.split('+').collect::<Vec<_>>();
        let name = tokens.pop().ok_or("Unsupported shortcut")?;
        let key = Key::from_name(name).ok_or("Unsupported shortcut")?;
        let mut modifiers = [Need::None; 4];
        for token in tokens {
            let Some(Key::Modifier(modifier, side)) = Key::from_name(token) else {
                return Err("Unsupported modifier".to_owned());
            };
            if modifiers[modifier.index()] != Need::None || key.modifier() == Some(modifier) {
                return Err("Duplicate shortcut modifier".to_owned());
            }

            modifiers[modifier.index()] = match side {
                Side::Left => Need::Either,
                Side::Right => Need::Right,
            };
        }

        Ok(Shortcut { key, modifiers })
    }

    pub fn key(&self) -> Key {
        self.key
    }

    pub fn uses(&self, modifiers: &[Modifier], key: Key) -> bool {
        self.key == key
            && modifiers
                .iter()
                .all(|modifier| self.modifiers[modifier.index()] != Need::None)
    }

    pub(super) fn key_matches(self, key: Key) -> bool {
        match (self.key, key) {
            (Key::Modifier(wanted, Side::Left), Key::Modifier(pressed, _)) => wanted == pressed,
            _ => self.key == key,
        }
    }

    pub(super) fn matches(self, key: Key, held: &Held, altgr_adds_ctrl: bool) -> bool {
        self.key_matches(key)
            && Modifier::ALL.into_iter().all(|modifier| {
                if key.modifier() == Some(modifier) {
                    return true;
                }

                let need = self.modifiers[modifier.index()];
                if altgr_adds_ctrl
                    && modifier == Modifier::Ctrl
                    && need == Need::None
                    && self.uses_right_alt()
                    && held.right(Modifier::Alt)
                {
                    return true;
                }

                let (left, right) = (held.left(modifier), held.right(modifier));
                match need {
                    Need::None => !left && !right,
                    Need::Either => left || right,
                    Need::Right => right,
                }
            })
    }

    pub(super) fn modifiers_held(self, held: &Held) -> bool {
        Modifier::ALL
            .into_iter()
            .all(|modifier| match self.modifiers[modifier.index()] {
                Need::None => true,
                Need::Either => held.left(modifier) || held.right(modifier),
                Need::Right => held.right(modifier),
            })
    }

    fn uses_right_alt(self) -> bool {
        self.modifiers[Modifier::Alt.index()] == Need::Right
            || self.key == Key::Modifier(Modifier::Alt, Side::Right)
    }

    fn physical_keys(self) -> Vec<Key> {
        match self.key {
            Key::Modifier(modifier, Side::Left) => vec![
                Key::Modifier(modifier, Side::Left),
                Key::Modifier(modifier, Side::Right),
            ],
            key => vec![key],
        }
    }
}

pub fn check_pair(hold: Shortcut, toggle: Shortcut, altgr_adds_ctrl: bool) -> Result<(), String> {
    for (first, second) in [(hold, toggle), (toggle, hold)] {
        if let Some(modifier) = first.key.modifier()
            && second.modifiers[modifier.index()] != Need::None
        {
            return Err(
                "Talk keys conflict: a modifier-only key starts the other combination".to_owned(),
            );
        }
    }

    for key in hold.physical_keys() {
        if !toggle.key_matches(key) {
            continue;
        }

        for combination in 0..256_u16 {
            let held = Held(std::array::from_fn(|index| combination & (1 << index) != 0));
            if hold.matches(key, &held, altgr_adds_ctrl)
                && toggle.matches(key, &held, altgr_adds_ctrl)
            {
                return Err("Talk keys must be different".to_owned());
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Shortcut {
        Shortcut::parse(text).unwrap()
    }

    #[test]
    fn parses_modifiers_and_rejects_bad_ones() {
        let shortcut = parse("Right Ctrl+Right Meta+F12");
        assert_eq!(shortcut.key(), Key::Function(12));
        assert_eq!(parse("L").key(), Key::Letter('L'));
        assert!(Shortcut::parse("Space+Ctrl").is_err());
        assert!(Shortcut::parse("Ctrl+Right Ctrl+Space").is_err());
        assert!(Shortcut::parse("Ctrl+Ctrl").is_err());
        assert!(Shortcut::parse("Ctrl+").is_err());
        assert!(Shortcut::parse("").is_err());
    }

    #[test]
    fn right_modifiers_need_the_right_key() {
        let shortcut = parse("Right Ctrl+Right Meta+F12");
        let mut held = Held::default();
        held.set(Key::Modifier(Modifier::Meta, Side::Right), true);
        held.set(Key::Modifier(Modifier::Ctrl, Side::Right), true);
        assert!(shortcut.matches(Key::Function(12), &held, true));
        let mut left = Held::default();
        left.set(Key::Modifier(Modifier::Ctrl, Side::Left), true);
        left.set(Key::Modifier(Modifier::Meta, Side::Left), true);
        assert!(!shortcut.matches(Key::Function(12), &left, true));
    }

    #[test]
    fn left_modifier_key_stands_for_either_side() {
        let shortcut = parse("Ctrl");
        assert!(shortcut.key_matches(Key::Modifier(Modifier::Ctrl, Side::Left)));
        assert!(shortcut.key_matches(Key::Modifier(Modifier::Ctrl, Side::Right)));
        let right = parse("Right Ctrl");
        assert!(!right.key_matches(Key::Modifier(Modifier::Ctrl, Side::Left)));
    }

    #[test]
    fn uses_matches_reserved_patterns_on_either_side() {
        assert!(parse("Meta+L").uses(&[Modifier::Meta], Key::Letter('L')));
        assert!(parse("Right Meta+L").uses(&[Modifier::Meta], Key::Letter('L')));
        assert!(parse("Ctrl+Alt+Delete").uses(&[Modifier::Ctrl, Modifier::Alt], Key::Delete));
        assert!(!parse("Ctrl+Delete").uses(&[Modifier::Ctrl, Modifier::Alt], Key::Delete));
        assert!(!parse("L").uses(&[Modifier::Meta], Key::Letter('L')));
    }

    #[test]
    fn pairs_that_one_press_triggers_together_conflict() {
        let check = |hold: &str, toggle: &str| check_pair(parse(hold), parse(toggle), true);
        assert!(check("Ctrl+Space", "Ctrl+Space").is_err());
        assert!(check("Ctrl", "Ctrl+Space").is_err());
        assert!(check("Right Alt", "Ctrl+Right Alt").is_err());
        assert!(check("Right Alt+Down", "Ctrl+Right Alt+Down").is_err());
        assert!(check("Ctrl", "Right Ctrl").is_err());
        assert!(check("Ctrl+Space", "Right Alt").is_ok());
        assert!(check("Ctrl+Space", "Ctrl+Shift+Space").is_ok());
        assert!(check("F8", "F9").is_ok());
    }
}
