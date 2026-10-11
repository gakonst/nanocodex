use std::sync::Arc;

use crate::Result;

#[derive(Clone, Default)]
pub(crate) struct ContextSourceConfig {
    execution_environment: Option<super::ExecutionEnvironment>,
}

impl ContextSourceConfig {
    pub(crate) fn set_execution_environment(&mut self, environment: super::ExecutionEnvironment) {
        self.execution_environment = Some(environment);
    }

    pub(crate) const fn execution_environment(&self) -> Option<&super::ExecutionEnvironment> {
        self.execution_environment.as_ref()
    }

    pub(crate) fn build(&self) -> ContextSource {
        ContextSource {
            execution_environment: self.execution_environment.clone(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct ContextSource {
    execution_environment: Option<super::ExecutionEnvironment>,
}

impl ContextSource {
    pub(crate) fn resolve_workspace(&self, requested: Option<&str>) -> Result<String> {
        Ok(requested.unwrap_or(".").to_owned())
    }

    pub(crate) fn project_instructions(&self, _workspace: &str) -> Option<String> {
        self.execution_environment
            .as_ref()?
            .project_instructions
            .as_deref()
            .map(str::to_owned)
    }

    pub(crate) const fn global_instructions(&self) -> Option<Arc<str>> {
        None
    }

    pub(crate) const fn execution_environment(&self) -> Option<&super::ExecutionEnvironment> {
        self.execution_environment.as_ref()
    }

    pub(crate) fn with_fallback_global(self, _fallback: Option<Arc<str>>) -> Self {
        self
    }
}
