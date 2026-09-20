use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

const COMMAND_MODIFIERS: KeyModifiers = KeyModifiers::CONTROL
    .union(KeyModifiers::ALT)
    .union(KeyModifiers::SUPER);

pub fn plain(key: KeyEvent) -> bool {
    !key.modifiers.intersects(COMMAND_MODIFIERS)
}

pub fn character(key: KeyEvent, character: char) -> bool {
    key.code == KeyCode::Char(character) && plain(key)
}

pub fn control(key: KeyEvent, character: char) -> bool {
    key.code == KeyCode::Char(character) && key.modifiers == KeyModifiers::CONTROL
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinguishes_text_commands_from_modified_keys() {
        assert!(character(
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
            'a'
        ));
        assert!(character(
            KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT),
            'A'
        ));
        assert!(!character(
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL),
            'a'
        ));
        assert!(control(
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL),
            'a'
        ));
        assert!(!control(
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::ALT),
            'a'
        ));
    }
}
