use super::{Result, invalid};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeSet;

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "camelCase", deny_unknown_fields)]
pub(super) enum Action {
    Observe {},
    #[serde(alias = "disconnect", alias = "cancel")]
    Release {},
    Shutdown {},
    Input {
        input: Input,
    },
    Click {
        x: f64,
        y: f64,
        #[serde(default)]
        button: u8,
    },
    Type {
        text: String,
    },
    Key {
        key: u16,
        #[serde(default)]
        modifiers: Vec<u16>,
    },
    Scroll {
        x: f64,
        y: f64,
        #[serde(rename = "deltaX")]
        delta_x: f64,
        #[serde(rename = "deltaY")]
        delta_y: f64,
    },
    Drag {
        x: f64,
        y: f64,
        #[serde(rename = "endX")]
        end_x: f64,
        #[serde(rename = "endY")]
        end_y: f64,
        #[serde(default)]
        button: u8,
        #[serde(rename = "durationMs", default = "drag_duration")]
        duration_ms: u16,
    },
}
fn drag_duration() -> u16 {
    300
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub(super) enum Input {
    Move {
        x: f64,
        y: f64,
    },
    Button {
        x: f64,
        y: f64,
        button: u8,
        down: bool,
    },
    Scroll {
        x: f64,
        y: f64,
        #[serde(rename = "deltaX")]
        delta_x: f64,
        #[serde(rename = "deltaY")]
        delta_y: f64,
    },
    Key {
        key: u16,
        down: bool,
    },
    Text {
        text: String,
    },
    ReleaseAll {},
}
fn point(x: f64, y: f64) -> Result<()> {
    if !x.is_finite() || !y.is_finite() || !(0.0..=1.0).contains(&x) || !(0.0..=1.0).contains(&y) {
        return Err(invalid("coordinates must be finite and within 0..=1"));
    }
    Ok(())
}
fn button(value: u8) -> Result<()> {
    if value > 2 {
        return Err(invalid("button must be 0 (left), 1 (right), or 2 (middle)"));
    }
    Ok(())
}
fn text(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 4096
        || value.contains('\0')
        || value
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return Err(invalid(
            "text must be 1..=4096 UTF-8 bytes without NUL or unsupported control characters",
        ));
    }
    Ok(())
}
fn key(value: u16) -> Result<()> {
    if hid_keysym(value).is_none() {
        return Err(invalid("unsupported HID keyboard usage"));
    }
    Ok(())
}
fn deltas(dx: f64, dy: f64) -> Result<()> {
    if !dx.is_finite() || !dy.is_finite() || dx.abs() > 4096.0 || dy.abs() > 4096.0 {
        return Err(invalid(
            "scroll deltas must be finite and within -4096..=4096",
        ));
    }
    Ok(())
}
impl Input {
    fn validate(&self) -> Result<()> {
        match self {
            Self::Move { x, y } => point(*x, *y),
            Self::Button {
                x, y, button: b, ..
            } => {
                point(*x, *y)?;
                button(*b)
            }
            Self::Scroll {
                x,
                y,
                delta_x,
                delta_y,
            } => {
                point(*x, *y)?;
                deltas(*delta_x, *delta_y)
            }
            Self::Key { key: k, .. } => key(*k),
            Self::Text { text: t } => text(t),
            Self::ReleaseAll {} => Ok(()),
        }
    }
}
pub(super) fn parse(value: Value) -> Result<Action> {
    let action: Action = serde_json::from_value(value)?;
    match &action {
        Action::Observe {} | Action::Release {} | Action::Shutdown {} => (),
        Action::Input { input } => input.validate()?,
        Action::Click { x, y, button: b } => {
            point(*x, *y)?;
            button(*b)?;
        }
        Action::Type { text: t } => text(t)?,
        Action::Key { key: k, modifiers } => {
            key(*k)?;
            let unique: BTreeSet<_> = modifiers.iter().copied().collect();
            if modifiers.len() > 4
                || unique.len() != modifiers.len()
                || modifiers.iter().any(|m| !(224..=231).contains(m) || m == k)
            {
                return Err(invalid(
                    "modifiers must be at most four distinct HID modifier usages",
                ));
            }
        }
        Action::Scroll {
            x,
            y,
            delta_x,
            delta_y,
        } => {
            point(*x, *y)?;
            deltas(*delta_x, *delta_y)?;
        }
        Action::Drag {
            x,
            y,
            end_x,
            end_y,
            button: b,
            duration_ms,
        } => {
            point(*x, *y)?;
            point(*end_x, *end_y)?;
            button(*b)?;
            if !(50..=1500).contains(duration_ms) {
                return Err(invalid("drag durationMs must be 50..=1500"));
            }
        }
    }
    Ok(action)
}

pub(super) fn x_button(button: u8) -> u8 {
    [1, 3, 2][usize::from(button)]
}

// USB HID keyboard page -> X keysyms. Keycodes are looked up in the server's
// mapping, never derived by adding the Linux evdev offset to a HID usage.
pub(super) fn hid_keysym(hid: u16) -> Option<u32> {
    Some(match hid {
        4..=29 => u32::from(hid - 4) + u32::from(b'a'),
        30..=38 => u32::from(hid - 30) + u32::from(b'1'),
        39 => u32::from(b'0'),
        40 => 0xff0d,
        41 => 0xff1b,
        42 => 0xff08,
        43 => 0xff09,
        44 => 0x20,
        45 => 0x2d,
        46 => 0x3d,
        47 => 0x5b,
        48 => 0x5d,
        49 | 50 => 0x5c,
        51 => 0x3b,
        52 => 0x27,
        53 => 0x60,
        54 => 0x2c,
        55 => 0x2e,
        56 => 0x2f,
        57 => 0xffe5,
        58..=69 => 0xffbe + u32::from(hid - 58),
        70 => 0xff61,
        71 => 0xff14,
        72 => 0xff13,
        73 => 0xff63,
        74 => 0xff50,
        75 => 0xff55,
        76 => 0xffff,
        77 => 0xff57,
        78 => 0xff56,
        79 => 0xff53,
        80 => 0xff51,
        81 => 0xff54,
        82 => 0xff52,
        83 => 0xff7f,
        84 => 0xffaf,
        85 => 0xffaa,
        86 => 0xffad,
        87 => 0xffab,
        88 => 0xff8d,
        89..=97 => 0xffb1 + u32::from(hid - 89),
        98 => 0xffb0,
        99 => 0xffae,
        100 => 0x3c,
        101 => 0xff67,
        103 => 0xffbd,
        104..=115 => 0xffca + u32::from(hid - 104),
        224 => 0xffe3,
        225 => 0xffe1,
        226 => 0xffe9,
        227 => 0xffeb,
        228 => 0xffe4,
        229 => 0xffe2,
        230 => 0xffea,
        231 => 0xffec,
        _ => return None,
    })
}
pub(super) fn find_keycode(first: u8, stride: u8, symbols: &[u32], symbol: u32) -> Option<u8> {
    if stride == 0 {
        return None;
    }
    // Prefer the unshifted column, then keypad/alternate columns. Raw modifiers
    // determine the active level; they are not synthesized for a physical key.
    let rows = || symbols.chunks_exact(usize::from(stride));
    let index = rows()
        .position(|row| row[0] == symbol)
        .or_else(|| rows().position(|row| row.contains(&symbol)))?;
    first.checked_add(u8::try_from(index).ok()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn rejects_invalid_inputs_before_any_events() {
        for value in [
            json!({"action":"click","x":1.01,"y":0}),
            json!({"action":"click","x":0,"y":0,"button":3}),
            json!({"action":"drag","x":0,"y":0,"endX":1,"endY":1,"durationMs":1501}),
            json!({"action":"key","key":4,"modifiers":[224,224]}),
            json!({"action":"key","key":4,"modifiers":[5]}),
            json!({"action":"key","key":65535}),
            json!({"action":"scroll","x":0,"y":0,"deltaX":4097,"deltaY":0}),
            json!({"action":"type","text":"\u{0}"}),
            json!({"action":"type","text":"x".repeat(4097)}),
            json!({"action":"input","input":{"kind":"button","x":0,"y":0,"button":0}}),
            json!({"action":"input","input":{"kind":"move","x":0,"y":0,"key":4}}),
            json!({"action":"observe","program":"sh"}),
        ] {
            assert!(parse(value.clone()).is_err(), "accepted {value}");
        }
        assert!(point(f64::NAN, 0.0).is_err());
        assert!(deltas(0.0, f64::INFINITY).is_err());
    }
    #[test]
    fn accepts_agent_and_raw_wire_shapes() {
        for value in [
            json!({"action":"click","x":0,"y":1}),
            json!({"action":"drag","x":0,"y":0,"endX":1,"endY":1}),
            json!({"action":"key","key":4,"modifiers":[224]}),
            json!({"action":"type","text":"Καλημέρα 世界\n"}),
            json!({"action":"input","input":{"kind":"key","key":225,"down":false}}),
            json!({"action":"input","input":{"kind":"scroll","x":0.5,"y":0.5,"deltaX":-4096,"deltaY":0}}),
            json!({"action":"input","input":{"kind":"releaseAll"}}),
            json!({"action":"disconnect"}),
            json!({"action":"shutdown"}),
        ] {
            assert!(parse(value.clone()).is_ok(), "rejected {value}");
        }
    }
    #[test]
    fn maps_hid_without_assuming_evdev_codes() {
        assert_eq!(hid_keysym(4), Some(0x61));
        assert_eq!(hid_keysym(40), Some(0xff0d));
        assert_eq!(hid_keysym(58), Some(0xffbe));
        assert_eq!(hid_keysym(82), Some(0xff52));
        assert_eq!(hid_keysym(224), Some(0xffe3));
        assert_eq!(hid_keysym(0), None);
        assert_eq!(find_keycode(8, 2, &[0x62, 0x61, 0x61, 0x41], 0x61), Some(9));
        assert_eq!(find_keycode(8, 0, &[], 0x61), None);
        assert_eq!([x_button(0), x_button(1), x_button(2)], [1, 3, 2]);
    }
}
