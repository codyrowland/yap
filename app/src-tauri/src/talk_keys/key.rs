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

    fn name(self) -> &'static str {
        match self {
            Modifier::Ctrl => "Ctrl",
            Modifier::Alt => "Alt",
            Modifier::Shift => "Shift",
            Modifier::Meta => "Meta",
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
    Letter(char),
    Digit(u8),
    Function(u8),
    Numpad(u8),
    Native(u32),
}

const NAMED: &[(Key, &str)] = &[
    (Key::Space, "Space"),
    (Key::Tab, "Tab"),
    (Key::Enter, "Enter"),
    (Key::Backspace, "Backspace"),
    (Key::Delete, "Delete"),
    (Key::Insert, "Insert"),
    (Key::Home, "Home"),
    (Key::End, "End"),
    (Key::PageUp, "PageUp"),
    (Key::PageDown, "PageDown"),
    (Key::Left, "Left"),
    (Key::Up, "Up"),
    (Key::Right, "Right"),
    (Key::Down, "Down"),
    (Key::Semicolon, "Semicolon"),
    (Key::Equals, "Equals"),
    (Key::Comma, "Comma"),
    (Key::Minus, "Minus"),
    (Key::Period, "Period"),
    (Key::Slash, "Slash"),
    (Key::Backtick, "Backtick"),
    (Key::LeftBracket, "LeftBracket"),
    (Key::Backslash, "Backslash"),
    (Key::RightBracket, "RightBracket"),
    (Key::Quote, "Quote"),
];

impl Key {
    pub fn modifier(self) -> Option<Modifier> {
        match self {
            Key::Modifier(modifier, _) => Some(modifier),
            _ => None,
        }
    }

    pub fn name(self) -> Option<String> {
        if let Some((_, name)) = NAMED.iter().find(|(key, _)| *key == self) {
            return Some((*name).to_owned());
        }

        match self {
            Key::Modifier(modifier, Side::Left) => Some(modifier.name().to_owned()),
            Key::Modifier(modifier, Side::Right) => Some(format!("Right {}", modifier.name())),
            Key::Letter(letter) => Some(letter.to_string()),
            Key::Digit(digit) => Some(digit.to_string()),
            Key::Function(number) => Some(format!("F{number}")),
            Key::Numpad(number) => Some(format!("Numpad{number}")),
            Key::Native(code) => Some(format!("VK-{code:02X}")),
            _ => None,
        }
    }

    pub fn from_name(name: &str) -> Option<Key> {
        if let Some((key, _)) = NAMED.iter().find(|(_, named)| *named == name) {
            return Some(*key);
        }

        let (side, modifier_name) = match name.strip_prefix("Right ") {
            Some(rest) => (Side::Right, rest),
            None => (Side::Left, name),
        };
        if let Some(modifier) = Modifier::ALL
            .into_iter()
            .find(|modifier| modifier.name() == modifier_name)
        {
            return Some(Key::Modifier(modifier, side));
        }

        if name.len() == 1 {
            let character = name.as_bytes()[0];
            if character.is_ascii_uppercase() {
                return Some(Key::Letter(character as char));
            }

            if character.is_ascii_digit() {
                return Some(Key::Digit(character - b'0'));
            }
        }

        if let Some(number) = name.strip_prefix('F')
            && let Ok(number) = number.parse::<u8>()
            && (1..=24).contains(&number)
        {
            return Some(Key::Function(number));
        }

        if let Some(number) = name.strip_prefix("Numpad")
            && let Ok(number) = number.parse::<u8>()
            && number <= 9
        {
            return Some(Key::Numpad(number));
        }

        if let Some(hex) = name.strip_prefix("VK-")
            && hex.len() == 2
            && let Ok(code) = u32::from_str_radix(hex, 16)
            && Key::Native(code).name().as_deref() == Some(name)
        {
            return Some(Key::Native(code));
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        let mut keys = vec![
            Key::Native(0x92),
            Key::Letter('A'),
            Key::Letter('Z'),
            Key::Digit(0),
            Key::Digit(9),
            Key::Function(1),
            Key::Function(24),
            Key::Numpad(0),
            Key::Numpad(9),
        ];
        keys.extend(NAMED.iter().map(|(key, _)| *key));
        for modifier in Modifier::ALL {
            keys.push(Key::Modifier(modifier, Side::Left));
            keys.push(Key::Modifier(modifier, Side::Right));
        }
        for key in keys {
            let name = key.name().unwrap();
            assert_eq!(Key::from_name(&name), Some(key), "{name}");
        }
    }

    #[test]
    fn names_match_saved_settings() {
        assert_eq!(
            Key::from_name("Right Alt"),
            Some(Key::Modifier(Modifier::Alt, Side::Right))
        );
        assert_eq!(
            Key::from_name("Ctrl"),
            Some(Key::Modifier(Modifier::Ctrl, Side::Left))
        );
        assert_eq!(Key::from_name("Space"), Some(Key::Space));
        assert_eq!(Key::from_name("F8"), Some(Key::Function(8)));
        assert_eq!(Key::Escape.name(), None);
    }

    #[test]
    fn rejects_unknown_names() {
        for name in [
            "",
            "a",
            "F0",
            "F25",
            "Numpad10",
            "Left Ctrl",
            "VK-1",
            "VK-xyz",
            "VK-0a",
            "Esc",
        ] {
            assert_eq!(Key::from_name(name), None, "{name}");
        }
    }
}
