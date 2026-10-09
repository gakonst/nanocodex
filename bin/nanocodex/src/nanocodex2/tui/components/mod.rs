// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

//! Stateful UI components and their event boundary.

mod actions;
mod app;
// Branch navigator overlay.
mod branch_navigator;
mod code_review;
mod composer;
mod context_diagnostics;
mod effort;
mod file_finder;
mod floating;
mod interaction;
mod keybindings;
mod model_selector;
mod node;
mod queue;
mod recent_prompt_picker;
mod review_confirmation;
mod root;
mod screen;
mod selection;
mod session_picker;
mod skill_picker;
mod subagent_tree_layout;
mod subagents;
mod theme_selector;
mod transcript;
mod waved_text;

pub(crate) use app::{AppEffect, AppEvent, AppNode};
pub(crate) use branch_navigator::BranchNavigator;
pub(crate) use interaction::{InteractionOutcome, InteractionOverlay};
pub(crate) use node::{ComponentUpdate, RenderRequest};
pub(crate) use queue::QueueId;
pub(crate) use root::{
    DraftReset, RecentPromptDraft, RestoredSessionProjection, RootEffect, RootNode, SessionListKind,
};
pub(crate) use transcript::image::{initialize as initialize_image_renderer, video_picker};
pub(crate) use transcript::math;
#[allow(
    unused_imports,
    reason = "used by the local driver's --tool-calls flag"
)]
pub(crate) use transcript::set_initial_tool_calls;

mod voice;

mod voice_menu;
