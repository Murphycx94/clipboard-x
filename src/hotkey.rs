//! Parsing the global shortcut the user types (`CmdOrCtrl+Shift+V`).

use global_hotkey::hotkey::{Code, HotKey, Modifiers};

/// The shortcut `text` names: modifiers (`CmdOrCtrl`, `Cmd`, `Ctrl`,
/// `Shift`, `Alt`/`Option`) and one key (a letter, a digit, `Space` or
/// `F1`-`F12`), joined by `+`. `CmdOrCtrl` is Command on macOS, Control
/// elsewhere. Errors say what is wrong, for the settings page.
pub fn parse(text: &str) -> Result<HotKey, String> {
    let mut modifiers = Modifiers::empty();
    let mut key = None;
    for part in text.split('+').map(str::trim) {
        if part.is_empty() {
            return Err("快捷键格式不正确".into());
        }
        match part.to_ascii_lowercase().as_str() {
            "cmdorctrl" | "commandorcontrol" => modifiers |= platform_modifier(),
            "cmd" | "command" | "super" | "meta" => modifiers |= Modifiers::META,
            "ctrl" | "control" => modifiers |= Modifiers::CONTROL,
            "shift" => modifiers |= Modifiers::SHIFT,
            "alt" | "option" => modifiers |= Modifiers::ALT,
            other => {
                if key.is_some() {
                    return Err("快捷键只能包含一个按键".into());
                }
                key = Some(code(other).ok_or_else(|| format!("不支持的按键：{part}"))?);
            }
        }
    }
    let key = key.ok_or("快捷键需要一个按键，例如 V")?;
    if modifiers.is_empty() {
        return Err("快捷键至少需要一个修饰键（Cmd/Ctrl/Shift/Alt）".into());
    }
    Ok(HotKey::new(Some(modifiers), key))
}

fn platform_modifier() -> Modifiers {
    if cfg!(target_os = "macos") {
        Modifiers::META
    } else {
        Modifiers::CONTROL
    }
}

fn code(key: &str) -> Option<Code> {
    const LETTERS: [Code; 26] = [
        Code::KeyA,
        Code::KeyB,
        Code::KeyC,
        Code::KeyD,
        Code::KeyE,
        Code::KeyF,
        Code::KeyG,
        Code::KeyH,
        Code::KeyI,
        Code::KeyJ,
        Code::KeyK,
        Code::KeyL,
        Code::KeyM,
        Code::KeyN,
        Code::KeyO,
        Code::KeyP,
        Code::KeyQ,
        Code::KeyR,
        Code::KeyS,
        Code::KeyT,
        Code::KeyU,
        Code::KeyV,
        Code::KeyW,
        Code::KeyX,
        Code::KeyY,
        Code::KeyZ,
    ];
    const DIGITS: [Code; 10] = [
        Code::Digit0,
        Code::Digit1,
        Code::Digit2,
        Code::Digit3,
        Code::Digit4,
        Code::Digit5,
        Code::Digit6,
        Code::Digit7,
        Code::Digit8,
        Code::Digit9,
    ];
    const FUNCTION: [Code; 12] = [
        Code::F1,
        Code::F2,
        Code::F3,
        Code::F4,
        Code::F5,
        Code::F6,
        Code::F7,
        Code::F8,
        Code::F9,
        Code::F10,
        Code::F11,
        Code::F12,
    ];
    let bytes = key.as_bytes();
    match bytes {
        [c @ b'a'..=b'z'] => Some(LETTERS[usize::from(c - b'a')]),
        [c @ b'0'..=b'9'] => Some(DIGITS[usize::from(c - b'0')]),
        _ if key == "space" => Some(Code::Space),
        [b'f', ..] => key[1..]
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=12).contains(n))
            .map(|n| FUNCTION[n - 1]),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_default() {
        let hotkey = parse("CmdOrCtrl+Shift+V").unwrap();
        assert_eq!(
            hotkey,
            HotKey::new(Some(platform_modifier() | Modifiers::SHIFT), Code::KeyV)
        );
    }

    #[test]
    fn parses_names_case_and_spaces() {
        assert_eq!(
            parse(" alt + space ").unwrap(),
            HotKey::new(Some(Modifiers::ALT), Code::Space)
        );
        assert_eq!(
            parse("Ctrl+Option+F12").unwrap(),
            HotKey::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::F12)
        );
        assert_eq!(
            parse("Cmd+1").unwrap(),
            HotKey::new(Some(Modifiers::META), Code::Digit1)
        );
    }

    #[test]
    fn rejects_bad_shortcuts() {
        assert!(parse("").is_err());
        assert!(parse("V").is_err());
        assert!(parse("Shift").is_err());
        assert!(parse("Shift+V+B").is_err());
        assert!(parse("Shift+Enter").is_err());
        assert!(parse("Shift+F13").is_err());
        assert!(parse("Shift++V").is_err());
    }
}
