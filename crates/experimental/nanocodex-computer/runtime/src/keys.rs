use crate::{Error, Result};
use serde_json::{Value, json};
/// CDP modifier bits: Alt=1, Control=2, Meta=4, Shift=8.
pub fn cdp_key(chord: &str) -> Result<Value> {
    let parts: Vec<_> = chord.split('+').collect();
    let mut modifiers = 0;
    for modifier in &parts[..parts.len().saturating_sub(1)] {
        modifiers |= match modifier.to_lowercase().as_str() {
            "alt" | "option" => 1,
            "ctrl" | "control" => 2,
            "super" | "cmd" | "command" | "meta" => 4,
            "shift" => 8,
            _ => return Err(Error::invalid("Unknown key modifier")),
        };
    }
    let name = parts.last().unwrap_or(&"").to_lowercase();
    let (key, code, vk) = match name.as_str() {
        "enter" | "return" => ("Enter".into(), "Enter".into(), 13),
        "tab" => ("Tab".into(), "Tab".into(), 9),
        "backspace" => ("Backspace".into(), "Backspace".into(), 8),
        "delete" => ("Delete".into(), "Delete".into(), 46),
        "escape" | "esc" => ("Escape".into(), "Escape".into(), 27),
        "space" => (" ".into(), "Space".into(), 32),
        "left" => ("ArrowLeft".into(), "ArrowLeft".into(), 37),
        "up" => ("ArrowUp".into(), "ArrowUp".into(), 38),
        "right" => ("ArrowRight".into(), "ArrowRight".into(), 39),
        "down" => ("ArrowDown".into(), "ArrowDown".into(), 40),
        "home" => ("Home".into(), "Home".into(), 36),
        "end" => ("End".into(), "End".into(), 35),
        "pageup" => ("PageUp".into(), "PageUp".into(), 33),
        "pagedown" => ("PageDown".into(), "PageDown".into(), 34),
        s if s.len() == 1 && s.as_bytes()[0].is_ascii_alphabetic() => {
            let upper = s.to_uppercase();
            (
                if modifiers & 8 != 0 {
                    upper.clone()
                } else {
                    s.into()
                },
                format!("Key{upper}"),
                upper.as_bytes()[0] as u32,
            )
        }
        s if s.len() == 1 && s.as_bytes()[0].is_ascii_digit() => {
            let digit = s.as_bytes()[0];
            let shifted = b")!@#$%^&*("[(digit - b'0') as usize] as char;
            (
                if modifiers & 8 != 0 {
                    shifted.to_string()
                } else {
                    s.into()
                },
                format!("Digit{s}"),
                digit as u32,
            )
        }
        s if s.starts_with('f') && s[1..].parse::<u32>().is_ok_and(|v| (1..=12).contains(&v)) => {
            let n = s[1..].parse::<u32>().unwrap();
            (format!("F{n}"), format!("F{n}"), 111 + n)
        }
        _ => {
            return Err(Error::invalid(
                "Unsupported key name; use typeText for arbitrary text",
            ));
        }
    };
    let text = if modifiers & 7 == 0 {
        match key.as_str() {
            "Enter" => "\r".into(),
            "Tab" => "\t".into(),
            s if s.chars().count() == 1 => s.into(),
            _ => String::new(),
        }
    } else {
        String::new()
    };
    let mut event = json!({"key":key,"code":code,"windowsVirtualKeyCode":vk,"modifiers":modifiers});
    if !text.is_empty() {
        event["text"] = json!(text);
        event["unmodifiedText"] = json!(name);
    }
    #[cfg(target_os = "macos")]
    if modifiers == 4 {
        let command = match name.as_str() {
            "a" => Some("selectAll"),
            "c" => Some("copy"),
            "x" => Some("cut"),
            "v" => Some("paste"),
            "z" => Some("undo"),
            _ => None,
        };
        if let Some(command) = command {
            event["commands"] = json!([command]);
        }
    }
    Ok(event)
}

/// Literal character input uses the captured US keyboard table without treating
/// uppercase, punctuation, or '+' as a chord. Other Unicode uses insertText.
pub(crate) fn cdp_text_character(character: char) -> Option<Value> {
    let (code, vk) = match character {
        'a'..='z' | 'A'..='Z' => {
            let upper = character.to_ascii_uppercase();
            (format!("Key{upper}"), upper as u32)
        }
        '0'..='9' => (format!("Digit{character}"), character as u32),
        ')' | '!' | '@' | '#' | '$' | '%' | '^' | '&' | '*' | '(' => {
            let digit = ")!@#$%^&*(".find(character).unwrap() as u8 + b'0';
            (format!("Digit{}", digit as char), digit as u32)
        }
        '\n' | '\r' => ("Enter".into(), 13),
        ' ' => ("Space".into(), 32),
        '`' | '~' => ("Backquote".into(), 192),
        '-' | '_' => ("Minus".into(), 189),
        '=' | '+' => ("Equal".into(), 187),
        '[' | '{' => ("BracketLeft".into(), 219),
        ']' | '}' => ("BracketRight".into(), 221),
        '\\' | '|' => ("Backslash".into(), 220),
        ';' | ':' => ("Semicolon".into(), 186),
        '\'' | '"' => ("Quote".into(), 222),
        ',' | '<' => ("Comma".into(), 188),
        '.' | '>' => ("Period".into(), 190),
        '/' | '?' => ("Slash".into(), 191),
        _ => return None,
    };
    let text = if matches!(character, '\n' | '\r') {
        "\r".into()
    } else {
        character.to_string()
    };
    let key = if code == "Enter" {
        "Enter".into()
    } else {
        character.to_string()
    };
    Some(
        json!({"type":"keyDown","modifiers":0,"windowsVirtualKeyCode":vk,"code":code,"key":key,"text":text,"unmodifiedText":text,"location":0,"isKeypad":false}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn literal_characters_match_all_132_captured_dispatches() {
        let oracle: Value =
            serde_json::from_str(include_str!("../tests/oracles/browser_sequential.json")).unwrap();
        assert_eq!(oracle["cases"].as_array().unwrap().len(), 132);
        for case in oracle["cases"].as_array().unwrap() {
            let character = case["character"].as_str().unwrap().chars().next().unwrap();
            let actual = if let Some(down) = cdp_text_character(character) {
                let mut up = down.clone();
                up["type"] = json!("keyUp");
                for key in ["text", "unmodifiedText", "isKeypad"] {
                    up.as_object_mut().unwrap().remove(key);
                }
                json!([{"method":"Input.dispatchKeyEvent","params":down},{"method":"Input.dispatchKeyEvent","params":up}])
            } else {
                json!([{"method":"Input.insertText","params":{"text":character.to_string()}}])
            };
            assert_eq!(actual, case["calls"], "{character:?}");
        }
    }
}
