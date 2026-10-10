// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

//! Pure transcript rendering for the Nanocodex terminal UI.
//!
//! Markdown layout (links, selections, inline images), syntax highlighting,
//! diffs, review findings, display math and the color theme. The crate has no
//! CLI, Hand or session dependencies, so its compiled output stays cached while
//! the rest of the terminal client changes.

pub mod diff;
pub mod format;
pub mod highlight;
pub mod image;
pub mod markdown;
pub mod math;
pub mod review;
pub mod theme;
