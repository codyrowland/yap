#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Modifier {
    Ctrl,
    Alt,
    Shift,
    Meta,
}

impl Modifier {
    pub const ALL: [Modifier; 4] = [
        Modifier::Ctrl,
        Modifier::Alt,
        Modifier::Shift,
        Modifier::Meta,
    ];

    pub(super) fn index(self) -> usize {
        self as usize
    }

    /// The W3C `key` name. Alone it means either side; with `Right` added
    /// it is the W3C `code` of the right-hand key.
    fn name(self) -> &'static str {
        match self {
            Modifier::Ctrl => "Control",
            Modifier::Alt => "Alt",
            Modifier::Shift => "Shift",
            Modifier::Meta => "Meta",
        }
    }

    fn legacy_name(self) -> &'static str {
        match self {
            Modifier::Ctrl => "Ctrl",
            other => other.name(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Side {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Key {
    Modifier(Modifier, Side),
    Escape,
    Space,
    Tab,
    Enter,
    Backspace,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    Left,
    Up,
    Right,
    Down,
    Semicolon,
    Equals,
    Comma,
    Minus,
    Period,
    Slash,
    Backtick,
    LeftBracket,
    Backslash,
    RightBracket,
    Quote,
    IntlBackslash,
    CapsLock,
    NumLock,
    ScrollLock,
    Pause,
    ContextMenu,
    NumpadAdd,
    NumpadSubtract,
    NumpadMultiply,
    NumpadDivide,
    NumpadDecimal,
    Letter(char),
    Digit(u8),
    Function(u8),
    Numpad(u8),
    /// A key with no name here; it can be pressed but not chosen.
    Native(u32),
}

/// Keys named by their W3C `code` (the name browsers report), with the name
/// Yap saved before settings version 7 where it differs.
const NAMED: &[(Key, &str, Option<&str>)] = &[
    (Key::Space, "Space", None),
    (Key::Tab, "Tab", None),
    (Key::Enter, "Enter", None),
    (Key::Backspace, "Backspace", None),
    (Key::Delete, "Delete", None),
    (Key::Insert, "Insert", None),
    (Key::Home, "Home", None),
    (Key::End, "End", None),
    (Key::PageUp, "PageUp", None),
    (Key::PageDown, "PageDown", None),
    (Key::Left, "ArrowLeft", Some("Left")),
    (Key::Up, "ArrowUp", Some("Up")),
    (Key::Right, "ArrowRight", Some("Right")),
    (Key::Down, "ArrowDown", Some("Down")),
    (Key::Semicolon, "Semicolon", None),
    (Key::Equals, "Equal", Some("Equals")),
    (Key::Comma, "Comma", None),
    (Key::Minus, "Minus", None),
    (Key::Period, "Period", None),
    (Key::Slash, "Slash", None),
    (Key::Backtick, "Backquote", Some("Backtick")),
    (Key::LeftBracket, "BracketLeft", Some("LeftBracket")),
    (Key::Backslash, "Backslash", None),
    (Key::RightBracket, "BracketRight", Some("RightBracket")),
    (Key::Quote, "Quote", None),
    (Key::IntlBackslash, "IntlBackslash", None),
    (Key::CapsLock, "CapsLock", None),
    (Key::NumLock, "NumLock", None),
    (Key::ScrollLock, "ScrollLock", None),
    (Key::Pause, "Pause", None),
    (Key::ContextMenu, "ContextMenu", None),
    (Key::NumpadAdd, "NumpadAdd", None),
    (Key::NumpadSubtract, "NumpadSubtract", None),
    (Key::NumpadMultiply, "NumpadMultiply", None),
    (Key::NumpadDivide, "NumpadDivide", None),
    (Key::NumpadDecimal, "NumpadDecimal", None),
];

impl Key {
    pub fn modifier(self) -> Option<Modifier> {
        match self {
            Key::Modifier(modifier, _) => Some(modifier),
            _ => None,
        }
    }

    /// The name saved in settings: the W3C `code`, except that a modifier
    /// without `Right` stands for either side.
    pub fn name(self) -> Option<String> {
        if let Some((_, name, _)) = NAMED.iter().find(|(key, ..)| *key == self) {
            return Some((*name).to_owned());
        }

        match self {
            Key::Modifier(modifier, Side::Left) => Some(modifier.name().to_owned()),
            Key::Modifier(modifier, Side::Right) => Some(format!("{}Right", modifier.name())),
            Key::Letter(letter) => Some(format!("Key{letter}")),
            Key::Digit(digit) => Some(format!("Digit{digit}")),
            Key::Function(number) => Some(format!("F{number}")),
            Key::Numpad(number) => Some(format!("Numpad{number}")),
            _ => None,
        }
    }

    pub fn from_name(name: &str) -> Option<Key> {
        if let Some((key, ..)) = NAMED.iter().find(|(_, named, _)| *named == name) {
            return Some(*key);
        }

        let (side, modifier_name) = match name.strip_suffix("Right") {
            Some(rest) => (Side::Right, rest),
            None => (Side::Left, name),
        };
        if let Some(modifier) = Modifier::ALL
            .into_iter()
            .find(|modifier| modifier.name() == modifier_name)
        {
            return Some(Key::Modifier(modifier, side));
        }

        if let Some(letter) = name.strip_prefix("Key")
            && let [character] = letter.as_bytes()
            && character.is_ascii_uppercase()
        {
            return Some(Key::Letter(*character as char));
        }

        if let Some(digit) = name.strip_prefix("Digit")
            && let [character] = digit.as_bytes()
            && character.is_ascii_digit()
        {
            return Some(Key::Digit(character - b'0'));
        }

        function_or_numpad(name)
    }

    /// Reads a key name saved before settings version 7 ("Right Alt", "L",
    /// "Equals"). Unnamed Windows keys ("VK-92") have no portable name.
    pub fn from_legacy_name(name: &str) -> Option<Key> {
        if let Some((key, ..)) = NAMED
            .iter()
            .find(|(_, named, legacy)| legacy.unwrap_or(named) == name)
        {
            return Some(*key);
        }

        let (side, modifier_name) = match name.strip_prefix("Right ") {
            Some(rest) => (Side::Right, rest),
            None => (Side::Left, name),
        };
        if let Some(modifier) = Modifier::ALL
            .into_iter()
            .find(|modifier| modifier.legacy_name() == modifier_name)
        {
            return Some(Key::Modifier(modifier, side));
        }

        if let [character] = name.as_bytes() {
            if character.is_ascii_uppercase() {
                return Some(Key::Letter(*character as char));
            }

            if character.is_ascii_digit() {
                return Some(Key::Digit(character - b'0'));
            }
        }

        function_or_numpad(name)
    }
}

fn function_or_numpad(name: &str) -> Option<Key> {
    if let Some(number) = name.strip_prefix('F')
        && let Ok(number) = number.parse::<u8>()
        && (1..=24).contains(&number)
        && name == format!("F{number}")
    {
        return Some(Key::Function(number));
    }

    if let Some(number) = name.strip_prefix("Numpad")
        && let [digit] = number.as_bytes()
        && digit.is_ascii_digit()
    {
        return Some(Key::Numpad(digit - b'0'));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_named_keys() -> Vec<Key> {
        let mut keys = vec![
            Key::Letter('A'),
            Key::Letter('Z'),
            Key::Digit(0),
            Key::Digit(9),
            Key::Function(1),
            Key::Function(24),
            Key::Numpad(0),
            Key::Numpad(9),
        ];
        keys.extend(NAMED.iter().map(|(key, ..)| *key));
        for modifier in Modifier::ALL {
            keys.push(Key::Modifier(modifier, Side::Left));
            keys.push(Key::Modifier(modifier, Side::Right));
        }
        keys
    }

    #[test]
    fn names_round_trip() {
        for key in all_named_keys() {
            let name = key.name().unwrap();
            assert_eq!(Key::from_name(&name), Some(key), "{name}");
        }
        assert_eq!(Key::Native(0x92).name(), None);
        assert_eq!(Key::Escape.name(), None);
    }

    #[test]
    fn names_are_w3c_codes() {
        for (name, key) in [
            ("AltRight", Key::Modifier(Modifier::Alt, Side::Right)),
            ("Control", Key::Modifier(Modifier::Ctrl, Side::Left)),
            ("Space", Key::Space),
            ("F8", Key::Function(8)),
            ("KeyL", Key::Letter('L')),
            ("Digit7", Key::Digit(7)),
            ("ArrowDown", Key::Down),
            ("Backquote", Key::Backtick),
        ] {
            assert_eq!(Key::from_name(name), Some(key), "{name}");
        }
    }

    #[test]
    fn rejects_unknown_names() {
        for name in [
            "",
            "a",
            "L",
            "Keyl",
            "KeyAB",
            "F0",
            "F25",
            "F08",
            "Numpad10",
            "Ctrl",
            "Right Alt",
            "ControlLeft",
            "VK-92",
            "Escape",
            "Left",
        ] {
            assert_eq!(Key::from_name(name), None, "{name}");
        }
    }

    #[test]
    fn legacy_names_map_to_the_same_keys() {
        for (legacy, key) in [
            ("Right Alt", Key::Modifier(Modifier::Alt, Side::Right)),
            ("Ctrl", Key::Modifier(Modifier::Ctrl, Side::Left)),
            ("Meta", Key::Modifier(Modifier::Meta, Side::Left)),
            ("Space", Key::Space),
            ("L", Key::Letter('L')),
            ("7", Key::Digit(7)),
            ("F8", Key::Function(8)),
            ("Numpad3", Key::Numpad(3)),
            ("Left", Key::Left),
            ("Equals", Key::Equals),
            ("Backtick", Key::Backtick),
            ("LeftBracket", Key::LeftBracket),
        ] {
            assert_eq!(Key::from_legacy_name(legacy), Some(key), "{legacy}");
        }
        assert_eq!(Key::from_legacy_name("VK-92"), None);
        assert_eq!(Key::from_legacy_name("Control"), None);
    }
}
