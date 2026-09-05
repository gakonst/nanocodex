// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

//! Build-kind projection used by the copied composer chrome.

pub(crate) struct InstallationKind;

impl InstallationKind {
    pub(crate) const fn is_development(&self) -> bool {
        cfg!(debug_assertions)
    }
}

pub(crate) const fn current() -> &'static InstallationKind {
    &InstallationKind
}
