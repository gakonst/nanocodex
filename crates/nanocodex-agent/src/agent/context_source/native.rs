use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use super::agents_md::{combine_instructions, load_global_instructions, load_instructions};
use crate::{NanocodexError, Result};

#[derive(Clone, Default)]
pub(crate) struct ContextSourceConfig {
    codex_home: Option<PathBuf>,
    execution_environment: Option<super::ExecutionEnvironment>,
}

impl ContextSourceConfig {
    pub(crate) fn set_codex_home(&mut self, codex_home: PathBuf) {
        self.codex_home = Some(codex_home);
    }

    pub(crate) fn codex_home(&self) -> Option<&Path> {
        self.codex_home.as_deref()
    }

    pub(crate) fn set_execution_environment(&mut self, environment: super::ExecutionEnvironment) {
        self.execution_environment = Some(environment);
    }

    pub(crate) const fn execution_environment(&self) -> Option<&super::ExecutionEnvironment> {
        self.execution_environment.as_ref()
    }

    pub(crate) fn build(&self) -> ContextSource {
        ContextSource {
            global_instructions: load_global_instructions(self.codex_home()),
            execution_environment: self.execution_environment.clone(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct ContextSource {
    global_instructions: Option<Arc<str>>,
    execution_environment: Option<super::ExecutionEnvironment>,
}

impl ContextSource {
    pub(crate) fn resolve_workspace(&self, requested: Option<&str>) -> Result<String> {
        if self.execution_environment.is_some() {
            // A configured execution environment means model-visible tools run
            // somewhere other than this process. Preserve its workspace name;
            // probing it through the embedding host would cross that boundary.
            return Ok(requested.unwrap_or(".").to_owned());
        }
        let requested = PathBuf::from(requested.unwrap_or("."));
        let resolved = std::fs::canonicalize(&requested).map_err(|source| {
            NanocodexError::ResolveWorkspace {
                path: requested,
                source,
            }
        })?;
        if !resolved.is_dir() {
            return Err(NanocodexError::WorkspaceNotDirectory { path: resolved });
        }
        resolved
            .into_os_string()
            .into_string()
            .map_err(|path| NanocodexError::WorkspaceNotUtf8 {
                path: PathBuf::from(path),
            })
    }

    pub(crate) fn project_instructions(&self, workspace: &str) -> Option<String> {
        if let Some(environment) = &self.execution_environment {
            combine_instructions(
                self.global_instructions.as_deref(),
                environment.project_instructions.as_deref(),
            )
        } else {
            load_instructions(Path::new(workspace), self.global_instructions.as_deref())
        }
    }

    pub(crate) fn global_instructions(&self) -> Option<Arc<str>> {
        self.global_instructions.as_ref().map(Arc::clone)
    }

    pub(crate) const fn execution_environment(&self) -> Option<&super::ExecutionEnvironment> {
        self.execution_environment.as_ref()
    }

    pub(crate) fn with_fallback_global(mut self, fallback: Option<Arc<str>>) -> Self {
        if self.global_instructions.is_none() {
            self.global_instructions = fallback;
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_project_snapshot_replaces_native_workspace_discovery() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("AGENTS.md"), "host instructions").unwrap();
        let mut config = ContextSourceConfig::default();
        config.set_execution_environment(
            super::super::ExecutionEnvironment::new("2026-07-29", "Etc/UTC")
                .project_instructions("guest instructions"),
        );

        assert_eq!(
            config
                .build()
                .project_instructions(workspace.path().to_str().unwrap())
                .as_deref(),
            Some("guest instructions")
        );
    }

    #[test]
    fn remote_workspace_is_not_resolved_through_the_native_host() {
        let mut config = ContextSourceConfig::default();
        config.set_execution_environment(super::super::ExecutionEnvironment::new(
            "2026-07-29",
            "Etc/UTC",
        ));

        assert_eq!(
            config
                .build()
                .resolve_workspace(Some("/workspace"))
                .unwrap(),
            "/workspace"
        );
    }
}
