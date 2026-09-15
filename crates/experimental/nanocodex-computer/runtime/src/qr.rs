//! Real local QR recognition and an explicit watch state machine. Recognition
//! failure is distinct from an image containing no symbol.
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::io::Cursor;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Symbol {
    pub payload: String,
    pub bounds: [u32; 4],
    pub mobile_url: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "status", content = "symbol", rename_all = "snake_case")]
pub enum Decode {
    Absent,
    Present(Symbol),
    Ambiguous,
    Failed,
}
pub fn decode(bytes: &[u8]) -> Result<Decode> {
    if bytes.len() > 8 * 1024 * 1024 {
        return Err(Error::invalid("QR image exceeds 8 MiB"));
    }
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| Error::invalid("Unrecognized QR image"))?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let image = reader
        .decode()
        .map_err(|_| Error::invalid("Cannot decode QR image within limits"))?
        .to_luma8();
    let (w, h) = image.dimensions();
    if w as u64 * h as u64 > 16_000_000 {
        return Err(Error::invalid("QR image exceeds pixel budget"));
    }
    let mut prepared =
        rqrr::PreparedImage::prepare_from_greyscale(w as usize, h as usize, |x, y| {
            image.get_pixel(x as u32, y as u32)[0]
        });
    let grids = prepared.detect_grids();
    if grids.is_empty() {
        return Ok(Decode::Absent);
    }
    if grids.len() != 1 {
        return Ok(Decode::Ambiguous);
    }
    let grid = &grids[0];
    let Ok((_, payload)) = grid.decode() else {
        return Ok(Decode::Failed);
    };
    if payload.is_empty() {
        return Ok(Decode::Failed);
    }
    let minx = grid
        .bounds
        .iter()
        .map(|p| p.x)
        .min()
        .unwrap()
        .clamp(0, w as i32);
    let miny = grid
        .bounds
        .iter()
        .map(|p| p.y)
        .min()
        .unwrap()
        .clamp(0, h as i32);
    let maxx = grid
        .bounds
        .iter()
        .map(|p| p.x)
        .max()
        .unwrap()
        .clamp(0, w as i32);
    let maxy = grid
        .bounds
        .iter()
        .map(|p| p.y)
        .max()
        .unwrap()
        .clamp(0, h as i32);
    if maxx <= minx || maxy <= miny {
        return Ok(Decode::Failed);
    }
    let mobile_url = mobile_url(&payload);
    Ok(Decode::Present(Symbol {
        payload,
        mobile_url,
        bounds: [
            minx as u32,
            miny as u32,
            (maxx - minx) as u32,
            (maxy - miny) as u32,
        ],
    }))
}
pub fn mobile_url(payload: &str) -> Option<String> {
    let u = url::Url::parse(payload).ok()?;
    if u.scheme() == "https"
        && u.host_str().is_some()
        && u.username().is_empty()
        && u.password().is_none()
    {
        Some(u.to_string())
    } else {
        None
    }
}
#[derive(Default, Clone, Debug, Serialize, Deserialize)]
pub struct Watch {
    pub frames: u64,
    pub absent: u8,
    pub page_changed: bool,
    pub terminal: bool,
    pub latest: Option<Symbol>,
}
impl Watch {
    pub fn needs_binding_check(&self) -> bool {
        self.frames.is_multiple_of(8)
    }
    /// Call only after a required binding check succeeded. A failed check should
    /// not advance frames. `changed` allows absent frames to finish QR-only flows.
    pub fn observe(
        &mut self,
        result: Decode,
        changed: bool,
        qr_only: bool,
    ) -> Result<Option<Symbol>> {
        if self.terminal {
            return Err(Error::action("QR watch already stopped"));
        }
        self.page_changed |= changed;
        self.frames += 1;
        match result {
            Decode::Present(symbol) => {
                self.absent = 0;
                if self.page_changed {
                    self.terminal = true;
                    return Err(Error::new(-32014, "page_changed"));
                }
                self.latest = Some(symbol.clone());
                Ok(Some(symbol))
            }
            Decode::Absent => {
                self.absent = self.absent.saturating_add(1);
                if qr_only && self.absent >= 3 {
                    self.terminal = true;
                }
                Ok(None)
            }
            Decode::Ambiguous | Decode::Failed => {
                self.absent = 0;
                Ok(None)
            }
        }
    }
    pub fn stop(&mut self) {
        self.terminal = true;
    }
}
