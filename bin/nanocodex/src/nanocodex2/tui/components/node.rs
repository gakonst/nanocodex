// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

//! Stateful component ownership and update results.

use crate::tui::theme::Theme;
use ratatui::{Frame, layout::Rect};

#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum RenderRequest {
    #[default]
    None,
    Streaming,
    Immediate,
}

pub(crate) struct ComponentUpdate<E> {
    pub(crate) effects: Vec<E>,
    pub(crate) render: RenderRequest,
}

impl<E> ComponentUpdate<E> {
    pub(crate) fn none() -> Self {
        Self {
            effects: Vec::new(),
            render: RenderRequest::None,
        }
    }

    pub(crate) fn render(render: RenderRequest) -> Self {
        Self {
            effects: Vec::new(),
            render,
        }
    }
}

pub(crate) trait Component {
    type Event;
    type Effect;

    fn update(&mut self, event: Self::Event) -> ComponentUpdate<Self::Effect>;

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme);
}

pub(crate) struct Node<C> {
    component: C,
}

impl<C> Node<C> {
    pub(crate) const fn new(component: C) -> Self {
        Self { component }
    }

    pub(crate) const fn component(&self) -> &C {
        &self.component
    }

    pub(crate) const fn component_mut(&mut self) -> &mut C {
        &mut self.component
    }
}

impl<C: Component> Node<C> {
    pub(crate) fn update(&mut self, event: C::Event) -> ComponentUpdate<C::Effect> {
        self.component.update(event)
    }

    pub(crate) fn render(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        self.component.render(frame, area, theme);
    }
}
