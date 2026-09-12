//! Original native page arithmetic, separated from event posting so its boundary
//! conditions can be verified without moving or operating a desktop.
use crate::{Error, Result};

/// Pixel deltas in CoreGraphics' x/y convention used by the recovered service.
/// 0x10014e59c/0x10014ead0 clamp the page dimension to at least 100 points;
/// 0x10015422c uses FRINTA (nearest, ties away), then applies direction signs.
/// 0x100722a64 requires each resulting axis to fit a signed 32-bit event field.
pub fn page_deltas(direction: &str, pages: f64, frame: [f64; 4]) -> Result<[i32; 2]> {
    if !pages.is_finite() || pages <= 0.0 {
        return Err(Error::invalid("pages must be finite and positive"));
    }
    let dimension = match direction {
        "up" | "down" => frame[3],
        "left" | "right" => frame[2],
        _ => return Err(Error::invalid("Invalid direction")),
    };
    if !dimension.is_finite() || dimension < 0.0 {
        return Err(Error::action("Invalid scroll target bounds"));
    }
    let magnitude = (pages * dimension.max(100.0)).round();
    // The original traps if integer conversion/event construction would overflow.
    // Return an explicit error before moving the pointer or posting any event.
    let [x, y] = match direction {
        "up" => [0.0, magnitude],
        "down" => [0.0, -magnitude],
        "left" => [-magnitude, 0.0],
        "right" => [magnitude, 0.0],
        _ => unreachable!(),
    };
    if [x, y]
        .iter()
        .any(|value| !value.is_finite() || *value < i32::MIN as f64 || *value > i32::MAX as f64)
    {
        return Err(Error::invalid(
            "Scroll delta exceeds signed 32-bit event range",
        ));
    }
    Ok([x as i32, y as i32])
}
