use super::execution::{
    finish_tool_execution_span, panicked_tool_output, record_tool_content, tool_execution_span,
};
use super::*;

#[derive(Clone)]
pub(crate) struct ToolRegistry {
    ordered: Vec<Arc<dyn Tool>>,
    definitions: Vec<ToolDefinition>,
    exposures: Vec<ToolExposure>,
    by_name: HashMap<Box<str>, usize>,
    pub(super) providers: Vec<Arc<dyn DynamicToolProvider>>,
}

impl ToolRegistry {
    pub(crate) fn contains(&self, name: &str) -> bool {
        self.get(name).is_some()
            || (!host_owned_name(name)
                && self
                    .providers
                    .iter()
                    .any(|provider| provider.contains(name)))
    }

    pub(crate) fn supports_parallel_tool_calls(&self, name: &str) -> bool {
        if let Some((handler, _)) = self.get(name) {
            return handler.supports_parallel_tool_calls();
        }
        self.providers
            .iter()
            .find(|provider| provider.contains(name))
            .is_some_and(|provider| provider.supports_parallel_tool_calls(name))
    }

    pub(super) async fn execute_direct(
        &self,
        name: &str,
        input: ToolInput,
        context: ToolContext<'_>,
    ) -> ToolOutput {
        let trace_content = tracing::enabled!(
            target: "nanocodex_tools",
            tracing::Level::INFO
        );
        let (arguments_kind, arguments) = match &input {
            ToolInput::Function(arguments) => ("function", arguments.get()),
            ToolInput::Freeform(arguments) => ("freeform", arguments.as_str()),
        };
        let arguments_content = trace_content.then(|| arguments.to_owned());
        let span = tool_execution_span(name, context, arguments.len(), arguments_kind, 1, "");
        if let Some(arguments_content) = &arguments_content {
            record_tool_content(&span, "tool.arguments", arguments_content);
        }
        let started_at = std::time::Instant::now();
        let dispatch = async {
            if let Some((handler, _definition)) = self.get(name) {
                return match handler.execute(input, context).await {
                    Ok(execution) => execution,
                    Err(error) => ToolOutput::error(error.to_string()),
                };
            }
            let provider_input = match input {
                ToolInput::Function(arguments) => {
                    match serde_json::from_str::<Value>(arguments.get()) {
                        Ok(arguments) => arguments,
                        Err(error) => {
                            return ToolOutput::error(format!(
                                "failed to decode {name} arguments: {error}"
                            ));
                        }
                    }
                }
                ToolInput::Freeform(_) => {
                    return ToolOutput::error(format!(
                        "dynamic tool {name} requires object arguments"
                    ));
                }
            };
            let Some(provider) = (!host_owned_name(name))
                .then(|| {
                    self.providers
                        .iter()
                        .find(|provider| provider.contains(name))
                })
                .flatten()
            else {
                return ToolOutput::error(format!("unsupported tool call: {name}"));
            };
            if let Some(execution) = provider.execute(name, provider_input, context).await {
                return execution;
            }
            ToolOutput::error(format!("unsupported tool call: {name}"))
        }
        .instrument(span.clone());
        let execution = match AssertUnwindSafe(dispatch).catch_unwind().await {
            Ok(execution) => execution,
            Err(payload) => panicked_tool_output(&span, payload),
        };
        let output_content = trace_content
            .then(|| serde_json::to_string(&execution.output).ok())
            .flatten();
        finish_tool_execution_span(&span, started_at, &execution, output_content.as_deref());
        execution
    }

    pub(crate) async fn execute_nested(
        &self,
        name: &str,
        input: Value,
        context: ToolContext<'_>,
    ) -> ToolOutput {
        let trace_content = tracing::enabled!(
            target: "nanocodex_tools",
            tracing::Level::INFO
        );
        let arguments_content = trace_content
            .then(|| serde_json::to_string(&input).ok())
            .flatten();
        let arguments_bytes = arguments_content.as_ref().map_or(0, String::len);
        let arguments_kind = match &input {
            Value::Null => "null",
            Value::Bool(_) => "boolean",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
        };
        let arguments_count = input.as_object().map_or_else(
            || input.as_array().map_or(1, Vec::len),
            serde_json::Map::len,
        );
        let argument_keys = trace_content
            .then(|| {
                input.as_object().map(|object| {
                    object
                        .keys()
                        .map(String::as_str)
                        .collect::<Vec<_>>()
                        .join(",")
                })
            })
            .flatten()
            .unwrap_or_default();
        let span = tool_execution_span(
            name,
            context,
            arguments_bytes,
            arguments_kind,
            arguments_count,
            &argument_keys,
        );
        if let Some(arguments_content) = &arguments_content {
            record_tool_content(&span, "tool.arguments", arguments_content);
        }
        let started_at = std::time::Instant::now();
        let dispatch = self
            .execute_nested_inner(name, input, context)
            .instrument(span.clone());
        let execution = match AssertUnwindSafe(dispatch).catch_unwind().await {
            Ok(execution) => execution,
            Err(payload) => panicked_tool_output(&span, payload),
        };
        let output_content = trace_content
            .then(|| serde_json::to_string(&execution.output).ok())
            .flatten();
        finish_tool_execution_span(&span, started_at, &execution, output_content.as_deref());
        execution
    }

    async fn execute_nested_inner(
        &self,
        name: &str,
        input: Value,
        context: ToolContext<'_>,
    ) -> ToolOutput {
        let Some((handler, definition)) = self.get_for_code_mode(name) else {
            let Some(provider) = self.providers.iter().find(|provider| {
                provider
                    .available_definitions()
                    .iter()
                    .any(|definition| definition.name() == name)
            }) else {
                return ToolOutput::error(format!("unsupported nested tool call: {name}"));
            };
            if let Some(execution) = provider.execute(name, input, context).await {
                return execution;
            }
            return ToolOutput::error(format!("unsupported nested tool call: {name}"));
        };
        let input = match definition {
            ToolDefinition::Function { .. } if !input.is_object() => {
                return ToolOutput::error(format!(
                    "nested function tool {name} requires an object argument"
                ));
            }
            ToolDefinition::Function { .. } => match to_raw_value(&input) {
                Ok(input) => ToolInput::Function(input),
                Err(error) => {
                    return ToolOutput::error(format!("failed to encode {name} input: {error}"));
                }
            },
            ToolDefinition::Custom { .. } => match input.as_str() {
                Some(input) => ToolInput::Freeform(input.to_owned()),
                None => {
                    return ToolOutput::error(format!(
                        "nested freeform tool {name} requires a string argument"
                    ));
                }
            },
            ToolDefinition::Namespace { .. } => {
                return ToolOutput::error(
                    "Responses namespace definitions cannot execute as nested Code Mode tools",
                );
            }
            ToolDefinition::ToolSearch { .. } => {
                return ToolOutput::error(
                    "provider-native tool_search cannot execute as a nested Code Mode tool",
                );
            }
        };
        match handler.execute(input, context).await {
            Ok(execution) => execution,
            Err(error) => ToolOutput::error(error.to_string()),
        }
    }

    pub(crate) fn nested_tool_metadata(&self) -> Vec<Value> {
        self.code_mode_definitions()
            .into_iter()
            .map(|definition| definition_metadata(definition.name(), &definition))
            .collect()
    }
    pub(super) fn from_ordered(ordered: Vec<Arc<dyn Tool>>) -> Self {
        let definitions = ordered
            .iter()
            .map(|tool| tool.definition())
            .collect::<Vec<_>>();
        let by_name = definitions
            .iter()
            .enumerate()
            .map(|(index, definition)| (definition.name().into(), index))
            .collect();
        Self {
            exposures: vec![ToolExposure::CodeModeOnly; definitions.len()],
            ordered,
            definitions,
            by_name,
            providers: Vec::new(),
        }
    }

    pub(super) fn set_all_exposures(&mut self, exposure: ToolExposure) {
        self.exposures.fill(exposure);
    }

    pub(super) fn extend(
        &mut self,
        tools: impl IntoIterator<Item = (Arc<dyn Tool>, ToolExposure)>,
    ) {
        for (tool, exposure) in tools {
            let definition = tool.definition();
            if self.by_name.contains_key(definition.name()) {
                tracing::warn!(
                    tool_name = definition.name(),
                    "skipping duplicate tool that is already registered"
                );
                continue;
            }
            let index = self.ordered.len();
            self.by_name.insert(definition.name().into(), index);
            self.definitions.push(definition);
            self.exposures.push(exposure);
            self.ordered.push(tool);
        }
    }

    fn get(&self, name: &str) -> Option<(&Arc<dyn Tool>, &ToolDefinition)> {
        let index = *self.by_name.get(name)?;
        Some((self.ordered.get(index)?, self.definitions.get(index)?))
    }

    fn get_for_code_mode(&self, name: &str) -> Option<(&Arc<dyn Tool>, &ToolDefinition)> {
        let index = *self.by_name.get(name)?;
        if !self.exposures.get(index)?.is_available_in_code_mode() {
            return None;
        }
        Some((self.ordered.get(index)?, self.definitions.get(index)?))
    }

    pub(crate) fn definitions(&self) -> &[ToolDefinition] {
        &self.definitions
    }

    pub(crate) fn direct_definitions(&self) -> impl Iterator<Item = &ToolDefinition> {
        self.definitions
            .iter()
            .zip(&self.exposures)
            .filter_map(|(definition, exposure)| exposure.is_direct().then_some(definition))
    }

    pub(crate) fn code_mode_tool_names(&self) -> Vec<(String, String)> {
        let mut names = self
            .code_mode_definitions()
            .into_iter()
            .map(|definition| {
                (
                    crate::selection::normalize_public_tool_name(definition.name()),
                    definition.name().to_owned(),
                )
            })
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    pub(crate) fn registered_code_mode_definitions(&self) -> Vec<ToolDefinition> {
        first_normalized_definitions(
            self.definitions
                .iter()
                .zip(&self.exposures)
                .filter(|(definition, exposure)| {
                    exposure.is_available_in_code_mode()
                        && !matches!(definition, ToolDefinition::ToolSearch { .. })
                })
                .map(|(definition, _)| definition.clone()),
        )
    }

    pub(crate) fn code_mode_tool_summaries(&self) -> Vec<(String, String)> {
        let mut seen = self
            .registered_code_mode_definitions()
            .into_iter()
            .map(|definition| crate::selection::normalize_public_tool_name(definition.name()))
            .collect::<HashSet<_>>();
        let mut summaries = self
            .providers
            .iter()
            .flat_map(|provider| provider.code_mode_tool_summaries())
            .filter_map(|(name, description)| {
                let normalized = crate::selection::normalize_public_tool_name(&name);
                (!host_owned_name(&name) && seen.insert(normalized.clone()))
                    .then_some((normalized, description))
            })
            .collect::<Vec<_>>();
        summaries.sort_by(|left, right| left.0.cmp(&right.0));
        summaries
    }

    pub(crate) fn code_mode_definitions(&self) -> Vec<ToolDefinition> {
        let definitions = self
            .definitions
            .iter()
            .zip(&self.exposures)
            .filter(|(_, exposure)| exposure.is_available_in_code_mode())
            .map(|(definition, _)| definition.clone())
            .chain(
                self.providers
                    .iter()
                    .flat_map(|provider| provider.available_definitions()),
            )
            .filter(|definition| {
                !matches!(definition, ToolDefinition::ToolSearch { .. })
                    && !host_owned_name(definition.name())
            });
        first_normalized_definitions(definitions)
    }

    #[cfg(test)]
    pub(super) fn entries(&self) -> impl Iterator<Item = (&Arc<dyn Tool>, &ToolDefinition)> {
        self.ordered.iter().zip(&self.definitions)
    }
}

fn first_normalized_definitions(
    definitions: impl IntoIterator<Item = ToolDefinition>,
) -> Vec<ToolDefinition> {
    let mut seen = HashSet::new();
    definitions
        .into_iter()
        .filter(|definition| {
            let normalized = crate::selection::normalize_public_tool_name(definition.name());
            if seen.insert(normalized.clone()) {
                true
            } else {
                tracing::warn!(
                    tool_name = definition.name(),
                    code_mode_name = normalized,
                    "skipping tool with a duplicate normalized Code Mode name"
                );
                false
            }
        })
        .collect()
}

fn definition_metadata(name: &str, definition: &ToolDefinition) -> Value {
    let kind = match definition {
        ToolDefinition::Function { .. } => "function",
        ToolDefinition::Namespace { .. } => "namespace",
        ToolDefinition::Custom { .. } => "freeform",
        ToolDefinition::ToolSearch { .. } => "tool_search",
    };
    let metadata_name = crate::selection::normalize_public_tool_name(name);
    json!({
        "name": metadata_name,
        "tool_name": name,
        "description": definition.description(),
        "kind": kind,
        "input_schema": definition.parameters().map(|schema| schema.as_value()),
        "output_schema": definition.output_schema().map(|schema| schema.as_value()),
    })
}
