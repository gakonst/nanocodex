//! Typed Waymote v2 records. Validate the entire event before writing any bytes.
use super::screen_gamepad::GamepadState;
use serde::Deserialize;
use serde_json::Value;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub(crate) enum Input {
    Move {
        x: f64,
        y: f64,
    },
    RelativeMove {
        #[serde(rename = "deltaX")]
        dx: f64,
        #[serde(rename = "deltaY")]
        dy: f64,
    },
    Button {
        x: Option<f64>,
        y: Option<f64>,
        button: u8,
        down: bool,
    },
    Scroll {
        x: Option<f64>,
        y: Option<f64>,
        #[serde(rename = "deltaX")]
        dx: f64,
        #[serde(rename = "deltaY")]
        dy: f64,
    },
    Key {
        key: u16,
        down: bool,
    },
    Text {
        text: String,
    },
    Gamepad {
        gamepad: GamepadState,
    },
    ReleaseAll {},
}
impl Input {
    pub(crate) fn parse(mut value: Value) -> Result<Self> {
        // Lease generation and sequence have already been checked by the publisher.
        // Agent-generated steps intentionally do not carry either field.
        if let Some(object) = value.as_object_mut() {
            object.remove("generation");
            object.remove("sequence");
        }
        let input: Self = serde_json::from_value(value)?;
        input.validate()?;
        Ok(input)
    }
    fn validate(&self) -> Result<()> {
        fn point(x: Option<f64>, y: Option<f64>) -> Result<()> {
            match (x, y) {
                (None, None) => Ok(()),
                (Some(x), Some(y))
                    if x.is_finite()
                        && y.is_finite()
                        && (0.0..=1.0).contains(&x)
                        && (0.0..=1.0).contains(&y) =>
                {
                    Ok(())
                }
                _ => Err("invalid pointer coordinates".into()),
            }
        }
        fn delta(dx: f64, dy: f64) -> Result<()> {
            if dx.is_finite() && dy.is_finite() && dx.abs() <= 4096.0 && dy.abs() <= 4096.0 {
                Ok(())
            } else {
                Err("invalid pointer delta".into())
            }
        }
        match self {
            Self::Move { x, y } => point(Some(*x), Some(*y)),
            Self::Button { x, y, button, .. } => {
                point(*x, *y)?;
                if *button <= 2 {
                    Ok(())
                } else {
                    Err("invalid button".into())
                }
            }
            Self::Scroll { x, y, dx, dy } => {
                point(*x, *y)?;
                delta(*dx, *dy)
            }
            Self::RelativeMove { dx, dy } => delta(*dx, *dy),
            Self::Key { key, .. } => hid_evdev(*key)
                .map(|_| ())
                .ok_or_else(|| "unsupported HID usage".into()),
            Self::Text { text } if text.is_empty() || text.len() > 4096 || text.contains('\0') => {
                Err("invalid text".into())
            }
            Self::Gamepad { gamepad } => gamepad.validate(),
            _ => Ok(()),
        }
    }
    pub(crate) fn records(&self, sequence: &mut u32) -> Vec<u8> {
        *sequence = sequence.wrapping_add(1).max(1);
        let mut out = Vec::new();
        let mut push =
            |kind, state, a, b, seq| out.extend_from_slice(&record(kind, state, a, b, seq));
        match self {
            Self::Move { x, y } => push(1, 0, coordinate(*x), coordinate(*y), *sequence),
            Self::Button { x, y, button, down } => {
                if let (Some(x), Some(y)) = (x, y) {
                    push(1, 0, coordinate(*x), coordinate(*y), *sequence);
                }
                push(2, u8::from(*down), 0x110 + u32::from(*button), 0, *sequence);
            }
            Self::Scroll { x, y, dx, dy } => {
                if let (Some(x), Some(y)) = (x, y) {
                    push(1, 0, coordinate(*x), coordinate(*y), *sequence);
                }
                push(
                    3,
                    0,
                    (-*dx as f32).to_bits(),
                    (-*dy as f32).to_bits(),
                    *sequence,
                );
            }
            Self::RelativeMove { dx, dy } => push(
                8,
                0,
                (*dx as f32).to_bits(),
                (*dy as f32).to_bits(),
                *sequence,
            ),
            Self::Key { key, down } => push(
                4,
                u8::from(*down),
                hid_evdev(*key).expect("validated HID"),
                0,
                *sequence,
            ),
            Self::ReleaseAll {} => push(5, 0, 0, 0, 0),
            Self::Text { text } => {
                let mut remaining = text.as_str();
                while !remaining.is_empty() {
                    let mut size = remaining.len().min(4000);
                    while !remaining.is_char_boundary(size) {
                        size -= 1;
                    }
                    out.extend_from_slice(&record(10, 0, size as u32, *sequence, 0));
                    out.extend_from_slice(&remaining.as_bytes()[..size]);
                    remaining = &remaining[size..];
                    if !remaining.is_empty() {
                        *sequence = sequence.wrapping_add(1).max(1);
                    }
                }
            }
            Self::Gamepad { .. } => {}
        }
        out
    }
}
fn coordinate(v: f64) -> u32 {
    (v * 65535.0).round() as u32
}
pub(crate) fn record(kind: u8, state: u8, a: u32, b: u32, sequence: u32) -> [u8; 16] {
    let mut wire = [0; 16];
    wire[0] = 2;
    wire[1] = kind;
    wire[2] = state;
    wire[4..8].copy_from_slice(&a.to_le_bytes());
    wire[8..12].copy_from_slice(&b.to_le_bytes());
    wire[12..].copy_from_slice(&sequence.to_le_bytes());
    wire
}
pub(crate) fn hid_evdev(hid: u16) -> Option<u32> {
    Some(match hid {
        4..=29 => [
            30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38, 50, 49, 24, 25, 16, 19, 31, 20, 22, 47,
            17, 45, 21, 44,
        ][usize::from(hid - 4)],
        30..=38 => u32::from(hid - 28),
        39 => 11,
        40 => 28,
        41 => 1,
        42 => 14,
        43 => 15,
        44 => 57,
        45 => 12,
        46 => 13,
        47 => 26,
        48 => 27,
        49 => 43,
        51 => 39,
        52 => 40,
        53 => 41,
        54 => 51,
        55 => 52,
        56 => 53,
        57 => 58,
        58..=67 => u32::from(hid + 1),
        68 => 87,
        69 => 88,
        73 => 110,
        74 => 102,
        75 => 104,
        76 => 111,
        77 => 107,
        78 => 109,
        79 => 106,
        80 => 105,
        81 => 108,
        82 => 103,
        83 => 69,
        84 => 98,
        85 => 55,
        86 => 74,
        87 => 78,
        88 => 96,
        89 => 79,
        90 => 80,
        91 => 81,
        92 => 75,
        93 => 76,
        94 => 77,
        95 => 71,
        96 => 72,
        97 => 73,
        98 => 82,
        99 => 83,
        100 => 86,
        103 => 117,
        224..=231 => [29, 42, 56, 125, 97, 54, 100, 126][usize::from(hid - 224)],
        _ => return None,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn relative_motion_does_not_release_held_button_and_release_is_sequence_zero() {
        let mut seq = u32::MAX;
        let held = Input::parse(json!({"kind":"button","button":1,"down":true}))
            .unwrap()
            .records(&mut seq);
        assert_eq!(held, record(2, 1, 0x111, 0, 1));
        let moved = Input::parse(json!({"kind":"relativeMove","deltaX":-12.5,"deltaY":4096}))
            .unwrap()
            .records(&mut seq);
        assert_eq!(
            moved,
            record(8, 0, (-12.5f32).to_bits(), 4096f32.to_bits(), 2)
        );
        assert_eq!(
            Input::ReleaseAll {}.records(&mut seq),
            record(5, 0, 0, 0, 0)
        );
    }
    #[test]
    fn rejects_invalid_inputs_before_records() {
        for value in [
            json!({"kind":"move","x":2,"y":0}),
            json!({"kind":"button","x":0,"button":0,"down":true}),
            json!({"kind":"key","key":65535,"down":true}),
            json!({"kind":"relativeMove","deltaX":4097,"deltaY":0}),
            json!({"kind":"relativeMove","deltaX":1,"deltaY":0,"x":0,"y":0}),
            json!({"kind":"releaseAll","key":4}),
            json!({"kind":"text","text":"\u{0}"}),
        ] {
            assert!(Input::parse(value.clone()).is_err(), "{value}");
        }
    }
    #[test]
    fn text_splits_at_utf8_boundaries_and_advances_sequence() {
        let text = format!("{}世界", "x".repeat(3999));
        let mut seq = 0;
        let bytes = Input::parse(json!({"kind":"text","text":text}))
            .unwrap()
            .records(&mut seq);
        assert_eq!(&bytes[..16], &record(10, 0, 3999, 1, 0));
        assert_eq!(&bytes[4015..4031], &record(10, 0, 6, 2, 0));
        assert_eq!(seq, 2);
    }
}
