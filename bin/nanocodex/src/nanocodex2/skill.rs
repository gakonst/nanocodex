// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

//! Model-facing skill metadata rendered by the copied skill picker.

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Skill {
    name: String,
    description: String,
}

impl Skill {
    #[cfg(test)]
    pub(crate) fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
        }
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn description(&self) -> &str {
        &self.description
    }
}
