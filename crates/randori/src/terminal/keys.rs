//! What a key sends to the shell. Printable text goes through the input
//! handler instead (so input methods can compose Hangul, kana, …); this covers
//! the keys that are not text: Enter, Backspace, arrows, Ctrl-letters and so on,
//! in xterm's encoding.

use alacritty_terminal::term::TermMode;
use gpui_kit::Keystroke;

/// The bytes for `keystroke`, or `None` when it is text (the input handler
/// sends it) or belongs to the app (⌘ shortcuts).
pub fn bytes_for(keystroke: &Keystroke, mode: TermMode) -> Option<Vec<u8>> {
    let m = &keystroke.modifiers;
    if m.platform || m.function && keystroke.key.len() == 1 {
        return None;
    }
    // xterm's modifier parameter: 1 + shift + 2·alt + 4·ctrl
    let param = 1 + u8::from(m.shift) + 2 * u8::from(m.alt) + 4 * u8::from(m.control);
    let app_cursor = mode.contains(TermMode::APP_CURSOR);
    let cursor = |letter: char| -> Vec<u8> {
        if param > 1 {
            format!("\x1b[1;{param}{letter}").into_bytes()
        } else if app_cursor {
            format!("\x1bO{letter}").into_bytes()
        } else {
            format!("\x1b[{letter}").into_bytes()
        }
    };
    let tilde = |code: u8| -> Vec<u8> {
        if param > 1 {
            format!("\x1b[{code};{param}~").into_bytes()
        } else {
            format!("\x1b[{code}~").into_bytes()
        }
    };
    let bytes = match keystroke.key.as_str() {
        "enter" if m.alt => b"\x1b\r".to_vec(),
        "enter" => b"\r".to_vec(),
        "tab" if m.shift => b"\x1b[Z".to_vec(),
        "tab" => b"\t".to_vec(),
        "escape" => b"\x1b".to_vec(),
        // ⌥⌫ deletes a word, as in macOS Terminal
        "backspace" if m.alt => b"\x1b\x7f".to_vec(),
        "backspace" if m.control => b"\x08".to_vec(),
        "backspace" => b"\x7f".to_vec(),
        // ⌥← / ⌥→ move by word (readline's ESC b / ESC f)
        "left" if m.alt && !m.control && !m.shift => b"\x1bb".to_vec(),
        "right" if m.alt && !m.control && !m.shift => b"\x1bf".to_vec(),
        "up" => cursor('A'),
        "down" => cursor('B'),
        "right" => cursor('C'),
        "left" => cursor('D'),
        "home" => cursor('H'),
        "end" => cursor('F'),
        "insert" => tilde(2),
        "delete" => tilde(3),
        "pageup" => tilde(5),
        "pagedown" => tilde(6),
        "f1" => cursor_ss3('P', param),
        "f2" => cursor_ss3('Q', param),
        "f3" => cursor_ss3('R', param),
        "f4" => cursor_ss3('S', param),
        "f5" => tilde(15),
        "f6" => tilde(17),
        "f7" => tilde(18),
        "f8" => tilde(19),
        "f9" => tilde(20),
        "f10" => tilde(21),
        "f11" => tilde(23),
        "f12" => tilde(24),
        "space" if m.control => vec![0],
        key if m.control => {
            let mut chars = key.chars();
            let (Some(c), None) = (chars.next(), chars.next()) else {
                return None;
            };
            let byte = control_byte(c)?;
            if m.alt { vec![0x1b, byte] } else { vec![byte] }
        }
        _ => return None,
    };
    Some(bytes)
}

/// F1–F4: `ESC O P` alone, `ESC [ 1 ; m P` with modifiers.
fn cursor_ss3(letter: char, param: u8) -> Vec<u8> {
    if param > 1 {
        format!("\x1b[1;{param}{letter}").into_bytes()
    } else {
        format!("\x1bO{letter}").into_bytes()
    }
}

/// Ctrl plus a key: letters and the punctuation that has a control code.
fn control_byte(c: char) -> Option<u8> {
    match c.to_ascii_lowercase() {
        c @ 'a'..='z' => Some(c as u8 & 0x1f),
        '@' | '2' => Some(0),
        '[' | '3' => Some(0x1b),
        '\\' | '4' => Some(0x1c),
        ']' | '5' => Some(0x1d),
        '^' | '6' => Some(0x1e),
        '_' | '/' | '7' | '-' => Some(0x1f),
        '8' | '?' => Some(0x7f),
        _ => None,
    }
}

/// Text pasted into the shell: line breaks as Enter, and wrapped in
/// bracketed-paste markers when the program asked for them, so a shell does
/// not run each pasted line as it arrives.
pub fn paste_bytes(text: &str, mode: TermMode) -> Vec<u8> {
    let text = text.replace("\r\n", "\r").replace('\n', "\r");
    if mode.contains(TermMode::BRACKETED_PASTE) {
        // a pasted end marker would end the paste early
        let text = text.replace("\x1b[201~", "");
        format!("\x1b[200~{text}\x1b[201~").into_bytes()
    } else {
        text.into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(spec: &'static str) -> Keystroke {
        Keystroke::parse(spec).unwrap()
    }

    fn sent(spec: &'static str, mode: TermMode) -> Option<Vec<u8>> {
        bytes_for(&key(spec), mode)
    }

    #[test]
    fn control_keys_send_their_codes() {
        let none = TermMode::empty();
        assert_eq!(sent("ctrl-c", none), Some(vec![3]));
        assert_eq!(sent("ctrl-d", none), Some(vec![4]));
        assert_eq!(sent("enter", none), Some(b"\r".to_vec()));
        assert_eq!(sent("backspace", none), Some(vec![0x7f]));
        assert_eq!(sent("escape", none), Some(vec![0x1b]));
        assert_eq!(sent("shift-tab", none), Some(b"\x1b[Z".to_vec()));
    }

    #[test]
    fn arrows_follow_the_cursor_mode() {
        assert_eq!(sent("up", TermMode::empty()), Some(b"\x1b[A".to_vec()));
        assert_eq!(sent("up", TermMode::APP_CURSOR), Some(b"\x1bOA".to_vec()));
        assert_eq!(
            sent("ctrl-right", TermMode::empty()),
            Some(b"\x1b[1;5C".to_vec())
        );
        assert_eq!(sent("alt-left", TermMode::empty()), Some(b"\x1bb".to_vec()));
    }

    #[test]
    fn text_and_app_shortcuts_are_not_keys() {
        assert_eq!(
            sent("a", TermMode::empty()),
            None,
            "text, via the input handler"
        );
        assert_eq!(sent("cmd-k", TermMode::empty()), None, "Randori's palette");
    }

    #[test]
    fn pastes_are_bracketed_when_asked() {
        assert_eq!(paste_bytes("a\nb", TermMode::empty()), b"a\rb".to_vec());
        assert_eq!(
            paste_bytes("ls\x1b[201~", TermMode::BRACKETED_PASTE),
            b"\x1b[200~ls\x1b[201~".to_vec()
        );
    }
}
