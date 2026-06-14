use tauri_plugin_global_shortcut::{Code, Modifiers, Shortcut};

/// Parse strings like "Super+Space" or "Ctrl+Alt+D".
pub fn parse_shortcut(s: &str) -> Option<Shortcut> {
    let mut mods = Modifiers::empty();
    let mut code: Option<Code> = None;
    for token in s.split('+').map(str::trim) {
        if token.is_empty() {
            return None;
        }
        match token.to_ascii_lowercase().as_str() {
            "super" | "meta" | "cmd" | "win" => mods |= Modifiers::SUPER,
            "ctrl" | "control" => mods |= Modifiers::CONTROL,
            "alt" | "option" => mods |= Modifiers::ALT,
            "shift" => mods |= Modifiers::SHIFT,
            "space" => code = Some(Code::Space),
            "enter" | "return" => code = Some(Code::Enter),
            "tab" => code = Some(Code::Tab),
            "escape" | "esc" => code = Some(Code::Escape),
            other if other.len() == 1 => {
                let c = other.chars().next()?.to_ascii_uppercase();
                code = char_to_code(c);
            }
            _ => return None,
        }
    }
    code.map(|c| Shortcut::new(Some(mods), c))
}

fn char_to_code(c: char) -> Option<Code> {
    Some(match c {
        'A' => Code::KeyA,
        'B' => Code::KeyB,
        'C' => Code::KeyC,
        'D' => Code::KeyD,
        'E' => Code::KeyE,
        'F' => Code::KeyF,
        'G' => Code::KeyG,
        'H' => Code::KeyH,
        'I' => Code::KeyI,
        'J' => Code::KeyJ,
        'K' => Code::KeyK,
        'L' => Code::KeyL,
        'M' => Code::KeyM,
        'N' => Code::KeyN,
        'O' => Code::KeyO,
        'P' => Code::KeyP,
        'Q' => Code::KeyQ,
        'R' => Code::KeyR,
        'S' => Code::KeyS,
        'T' => Code::KeyT,
        'U' => Code::KeyU,
        'V' => Code::KeyV,
        'W' => Code::KeyW,
        'X' => Code::KeyX,
        'Y' => Code::KeyY,
        'Z' => Code::KeyZ,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_super_space() {
        let sc = parse_shortcut("Super+Space").unwrap();
        assert_eq!(sc.mods, Modifiers::SUPER);
        assert_eq!(sc.key, Code::Space);
    }

    #[test]
    fn parses_ctrl_alt_d_case_insensitive() {
        let sc = parse_shortcut("ctrl+ALT+d").unwrap();
        assert_eq!(sc.mods, Modifiers::CONTROL | Modifiers::ALT);
        assert_eq!(sc.key, Code::KeyD);
    }

    #[test]
    fn parses_named_keys() {
        for (s, code) in [
            ("Enter", Code::Enter),
            ("escape", Code::Escape),
            ("tab", Code::Tab),
        ] {
            let sc = parse_shortcut(s).unwrap();
            assert_eq!(sc.key, code, "input {s}");
            assert_eq!(sc.mods, Modifiers::empty(), "input {s}");
        }
    }

    #[test]
    fn aliases_map_to_super() {
        for s in ["Super+A", "Meta+A", "Cmd+A", "Win+A"] {
            let sc = parse_shortcut(s).unwrap();
            assert_eq!(sc.mods, Modifiers::SUPER, "input {s}");
            assert_eq!(sc.key, Code::KeyA);
        }
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_shortcut("").is_none());
        assert!(parse_shortcut("Super+").is_none());
        assert!(parse_shortcut("Foo+Space").is_none());
        assert!(parse_shortcut("Super+NotAKey").is_none());
    }

    #[test]
    fn rejects_modifiers_only() {
        assert!(parse_shortcut("Ctrl+Alt").is_none());
    }
}
