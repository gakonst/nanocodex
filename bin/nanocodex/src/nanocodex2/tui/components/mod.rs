// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

//! Stateful UI components and their event boundary.

mod actions;
mod app;
mod composer;
mod context_diagnostics;
mod effort;
mod file_finder;
mod floating;
mod keybindings;
mod model_selector;
mod node;
mod queue;
mod recent_prompt_picker;
mod review_confirmation;
mod root;
mod selection;
mod session_picker;
mod skill_picker;
mod subagent_tree_layout;
mod subagents;
mod theme_selector;
mod transcript;
mod waved_text;

pub(crate) use app::{AppEffect, AppEvent, AppNode};
pub(crate) use node::{ComponentUpdate, RenderRequest};
pub(crate) use queue::QueueId;
pub(crate) use root::{
    DraftReset, RecentPromptDraft, RestoredSessionProjection, RootEffect, RootNode, SessionListKind,
};
pub(crate) use transcript::image::initialize as initialize_image_renderer;
