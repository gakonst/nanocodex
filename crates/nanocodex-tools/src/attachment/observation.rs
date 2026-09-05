//! Demand-driven screen observations, independent of durable tool execution.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::Serialize;

use super::{AttachmentError, valid_safe_identifier};

/// Maximum decoded image size accepted by every attachment implementation.
pub const MAX_OBSERVATION_IMAGE_BYTES: usize = 180_000;
/// Capture deadline shared with the managed observation broker.
pub const OBSERVATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Physical or virtual screen represented by a hand.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationKind {
    /// Host desktop.
    Desktop,
    /// Browser viewport.
    Browser,
    /// Phone display.
    Phone,
}

/// Immutable, non-secret screen description.
#[derive(Clone, Debug, Serialize)]
pub struct ObservationSurface {
    id: String,
    name: String,
    kind: ObservationKind,
}

impl ObservationSurface {
    /// Creates a bounded screen description.
    ///
    /// # Errors
    /// Rejects invalid identifiers and empty or oversized names.
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        kind: ObservationKind,
    ) -> Result<Self, AttachmentError> {
        let id = id.into();
        let name = name.into();
        if !valid_safe_identifier(&id, 128) || name.trim().is_empty() || name.len() > 128 {
            return Err(AttachmentError::Catalog(
                "invalid observation surface".into(),
            ));
        }
        Ok(Self { id, name, kind })
    }

    /// Stable screen identifier within this attachment.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }
}

/// Supported inert image formats; no URLs, SVG, or executable content.
#[derive(Clone, Copy, Debug, Serialize)]
pub enum ObservationImageFormat {
    /// JPEG image.
    #[serde(rename = "image/jpeg")]
    Jpeg,
    /// PNG image.
    #[serde(rename = "image/png")]
    Png,
}

/// One bounded image captured in response to a viewer request.
#[derive(Debug, Serialize)]
pub struct ObservationFrame {
    captured_at: u64,
    width: u32,
    height: u32,
    mime_type: ObservationImageFormat,
    data: String,
}

impl ObservationFrame {
    /// Encodes an image for the cross-language observation protocol.
    ///
    /// # Errors
    /// Rejects oversized or empty images, dimensions, and non-JavaScript timestamps.
    pub fn new(
        captured_at: u64,
        width: u32,
        height: u32,
        format: ObservationImageFormat,
        bytes: &[u8],
    ) -> Result<Self, AttachmentError> {
        if captured_at > 9_007_199_254_740_991
            || !(1..=8192).contains(&width)
            || !(1..=8192).contains(&height)
            || bytes.is_empty()
            || bytes.len() > MAX_OBSERVATION_IMAGE_BYTES
        {
            return Err(AttachmentError::Catalog("invalid observation frame".into()));
        }
        Ok(Self {
            captured_at,
            width,
            height,
            mime_type: format,
            data: STANDARD.encode(bytes),
        })
    }
}

/// Application-owned capture capability. Capture futures must be cancellation
/// safe: dropping one must stop capture and release its resources. The driver
/// permits one capture at a time and drops it on timeout, cancellation, or detach.
#[async_trait::async_trait]
pub trait ObservationProvider: Send + Sync + 'static {
    /// Descriptors snapshotted once when the attachment starts (1–8 unique IDs).
    fn surfaces(&self) -> &[ObservationSurface];
    /// Captures the requested surface. Errors are redacted before transmission.
    async fn capture(&self, surface_id: &str) -> Result<ObservationFrame, AttachmentError>;
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum ObservationResult {
    Frame { frame: ObservationFrame },
    Unavailable { message: &'static str },
}

impl ObservationResult {
    pub(crate) const fn unavailable() -> Self {
        Self::Unavailable {
            message: "Screen capture unavailable",
        }
    }
}
