//! Recovered native screenshot sizing and input arithmetic. This module never
//! reads a desktop; callers supply geometry and private, trusted configuration.
use crate::{Error, Result};

#[cfg(target_os = "macos")]
#[path = "screenshot_macos.rs"]
pub(crate) mod macos;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Encoding {
    Jpeg { quality: Option<f64> },
    Png,
}
impl Encoding {
    pub fn mime_type(self) -> &'static str {
        match self {
            Self::Jpeg { .. } => "image/jpeg",
            Self::Png => "image/png",
        }
    }
}

/// This is provider configuration, not a model-facing Sky argument or vendor
/// remote configuration client. Defaults are recovered from 0x10020ac14.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Configuration {
    pub normalize_to_points: bool,
    pub encoding: Encoding,
}
impl Default for Configuration {
    fn default() -> Self {
        Self::from_flags(true, true, 0.8)
    }
}
impl Configuration {
    pub fn from_flags(normalize_to_points: bool, jpeg: bool, quality: f64) -> Self {
        Self {
            normalize_to_points,
            encoding: if jpeg {
                Encoding::Jpeg {
                    quality: (quality > 0.0 && quality <= 1.0).then_some(quality),
                }
            } else {
                Encoding::Png
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Size {
    pub effective_scale: f64,
    pub width: f64,
    pub height: f64,
}
/// 0x100082bf8 returns all three doubles. Size rounding happens later and must
/// never be used to reconstruct the coordinate scale.
pub fn normalize_size(pixels: [f64; 2], display_scale: f64, normalize: bool) -> Result<Size> {
    if !pixels.into_iter().all(f64::is_finite) || !display_scale.is_finite() || display_scale <= 0.0
    {
        return Err(Error::action("Invalid screenshot geometry"));
    }
    let base = if normalize && display_scale > 1.0 {
        display_scale
    } else {
        1.0
    };
    let [width, height] = pixels.map(|n| n / base);
    if width <= 0.0 || height <= 0.0 {
        return Ok(Size {
            effective_scale: base,
            width: 1.0,
            height: 1.0,
        });
    }
    let first = (2048.0 / width.max(height)).min(1.0);
    let second = (768.0 / (width * first).min(height * first)).min(1.0);
    let factor = first * second;
    let result = Size {
        effective_scale: base / factor,
        width: width * factor,
        height: height * factor,
    };
    if ![result.effective_scale, result.width, result.height]
        .into_iter()
        .all(f64::is_finite)
        || result.width <= 0.0
        || result.height <= 0.0
    {
        return Err(Error::action("Screenshot geometry cannot be represented"));
    }
    Ok(result)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    pub frame: [f64; 4],
    pub display_scale: f64,
    pub input_scale: f64,
    pub unrounded_size: [f64; 2],
    pub pixels: [usize; 2],
}
impl Geometry {
    pub fn new(frame: [f64; 4], display_scale: f64, config: Configuration) -> Result<Self> {
        valid_frame(frame)?;
        let size = normalize_size(
            [frame[2] * display_scale, frame[3] * display_scale],
            display_scale,
            config.normalize_to_points,
        )?;
        let dimensions = [size.width.ceil(), size.height.ceil()];
        if dimensions.into_iter().any(|n| !(1.0..=2048.0).contains(&n))
            || dimensions[0] * dimensions[1] > 2048.0 * 768.0
        {
            return Err(Error::action("Invalid or oversized screenshot dimensions"));
        }
        let input_scale = size.effective_scale / display_scale;
        if !input_scale.is_finite() || input_scale <= 0.0 {
            return Err(Error::action("Invalid screenshot input scale"));
        }
        Ok(Self {
            frame,
            display_scale,
            input_scale,
            unrounded_size: [size.width, size.height],
            pixels: dimensions.map(|n| n as usize),
        })
    }
    /// Incoming native command doubles become Int64 (FCVTZS) before scaling.
    /// Out-of-image and overflow errors are explicit replacement safeguards.
    pub fn screen_point(&self, point: [f64; 2]) -> Result<[f64; 2]> {
        let point = integer_point(point)?;
        if point
            .into_iter()
            .enumerate()
            .any(|(axis, value)| value < 0.0 || value >= self.pixels[axis] as f64)
        {
            return Err(Error::invalid("Point outside valid screenshot bounds"));
        }
        let result = [
            self.frame[0] + point[0] * self.input_scale,
            self.frame[1] + point[1] * self.input_scale,
        ];
        if !result.into_iter().all(f64::is_finite) {
            return Err(Error::invalid("Screenshot point cannot be represented"));
        }
        Ok(result)
    }
}
fn integer_point(point: [f64; 2]) -> Result<[f64; 2]> {
    // ±2^63 is exactly representable; the upper bound is exclusive.
    if point.into_iter().any(|n| {
        !n.is_finite() || !(-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&n)
    }) {
        return Err(Error::invalid(
            "Screenshot coordinate exceeds native integer range",
        ));
    }
    Ok(point.map(f64::trunc))
}
fn valid_frame(frame: [f64; 4]) -> Result<()> {
    if !frame.into_iter().all(f64::is_finite)
        || frame[2] <= 0.0
        || frame[3] <= 0.0
        || !(frame[0] + frame[2]).is_finite()
        || !(frame[1] + frame[3]).is_finite()
    {
        return Err(Error::action("Invalid screenshot window frame"));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq)]
pub struct Screen {
    /// Native Cocoa coordinates. The ordered list begins with the primary screen.
    pub frame: [f64; 4],
    pub backing_scale: f64,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Displays(pub Vec<Screen>);
impl Displays {
    pub fn scale_for_window(&self, frame: [f64; 4]) -> Result<f64> {
        valid_frame(frame)?;
        if self.0.len() > 64 {
            return Err(Error::action("Too many screenshot displays"));
        }
        let Some(primary) = self.0.first() else {
            return Ok(2.0);
        };
        // AX/CoreGraphics use a top-left primary origin; NSScreen uses bottom-left.
        let cocoa = [
            frame[0],
            primary.frame[1] + primary.frame[3] - frame[1] - frame[3],
            frame[2],
            frame[3],
        ];
        valid_frame(cocoa)?;
        let mut best = None;
        for screen in &self.0 {
            valid_frame(screen.frame)?;
            if !screen.backing_scale.is_finite() || screen.backing_scale <= 0.0 {
                return Err(Error::action("Invalid screenshot display scale"));
            }
            let [x, y, w, h] = screen.frame;
            let width = ((x + w).min(cocoa[0] + cocoa[2]) - x.max(cocoa[0])).max(0.0);
            let height = ((y + h).min(cocoa[1] + cocoa[3]) - y.max(cocoa[1])).max(0.0);
            let area = width * height;
            if !area.is_finite() {
                return Err(Error::action("Display intersection exceeds bounds"));
            }
            if best.is_none_or(|(prior, _)| area > prior) {
                best = Some((area, screen.backing_scale));
            }
        }
        Ok(best.unwrap().1)
    }
}

/// Geometry publication is separate from capture. Preview-only captures cannot
/// grant or change model-coordinate authority. Generation receipts reject stale
/// asynchronous completion even if a future provider parallelizes captures.
#[derive(Clone, Debug, Default)]
pub struct Publication {
    generation: u64,
    current: Option<(Geometry, Displays)>,
}
impl Publication {
    pub fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.current = None;
    }
    pub fn begin(&mut self) -> u64 {
        self.invalidate();
        self.generation
    }
    pub fn commit(&mut self, receipt: u64, geometry: Geometry, displays: Displays) -> Result<()> {
        if receipt != self.generation {
            return Err(Error::action(
                "Screenshot observation was invalidated during capture",
            ));
        }
        self.current = Some((geometry, displays));
        Ok(())
    }
    pub fn current(&self, frame: [f64; 4], displays: &Displays) -> Result<Geometry> {
        let (geometry, captured_displays) = self.current.as_ref().ok_or_else(|| {
            Error::action("Query get_app_state before using screenshot coordinates")
        })?;
        if geometry.frame != frame || displays != captured_displays {
            return Err(Error::action(
                "Window or display geometry changed; query get_app_state again",
            ));
        }
        Ok(*geometry)
    }
}
