// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

//! Configurable terminal colors and light/dark mode selection.
//!
//! The palette lives in `nanocodex-tui-render`; this module adds colors for
//! CLI-owned types and system light/dark detection.

use crate::nanocodex2::config::ReasoningEffort;
use nanocodex_managed::ManagedModel as Model;
use ratatui::style::Color;
use tokio::{sync::mpsc, time::Duration};
use tokio_util::sync::CancellationToken;

pub(crate) use nanocodex_tui_render::theme::{ColorScheme, Theme, ThemeMode};

/// Colors for reasoning effort and model identity.
pub(crate) trait ThemeExt {
    fn effort(&self, effort: ReasoningEffort) -> Color;
    fn model(&self, model: Model) -> Color;
}

impl ThemeExt for Theme {
    fn effort(&self, effort: ReasoningEffort) -> Color {
        match effort {
            ReasoningEffort::Low => self.thinking_low(),
            ReasoningEffort::Medium => self.thinking_medium(),
            ReasoningEffort::High => self.thinking_high(),
            ReasoningEffort::Xhigh => self.thinking_xhigh(),
            ReasoningEffort::Max => self.thinking_max(),
        }
    }

    fn model(&self, model: Model) -> Color {
        match model {
            Model::Oai(nanocodex::Model::Luna) => Color::White,
            Model::Oai(nanocodex::Model::Sol) => Color::Yellow,
            Model::Oai(nanocodex::Model::Astra) => Color::LightMagenta,
            _ => Color::White,
        }
    }
}

pub(crate) fn detect_system_scheme() -> Option<ColorScheme> {
    match dark_light::detect().ok()? {
        dark_light::Mode::Light => Some(ColorScheme::Light),
        dark_light::Mode::Dark => Some(ColorScheme::Dark),
        dark_light::Mode::Unspecified => None,
    }
}

const SYSTEM_SCHEME_POLL_INTERVAL: Duration = Duration::from_millis(100);

pub(crate) fn watch_system_scheme(
    updates: mpsc::UnboundedSender<ColorScheme>,
    shutdown: CancellationToken,
) {
    tokio::spawn(async move {
        let mut last = None;
        let mut interval = tokio::time::interval(SYSTEM_SCHEME_POLL_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = interval.tick() => {
                    let detected = tokio::task::spawn_blocking(detect_system_scheme)
                        .await
                        .ok()
                        .flatten();
                    if let Some(scheme) = detected
                        && last != Some(scheme)
                    {
                        last = Some(scheme);
                        if updates.send(scheme).is_err() {
                            break;
                        }
                    }
                }
            }
        }
    });
}
