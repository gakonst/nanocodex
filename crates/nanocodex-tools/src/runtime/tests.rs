use std::{
    ffi::OsString,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use nanocodex_oai_api::{auth::OpenAiAuth, responses::JsonSchema, tools::ToolDefinition};
use serde::Deserialize;
use serde_json::{Value, json, value::to_raw_value};
use tempfile::tempdir;

use crate::{ToolOutputBody, ToolResult, WorkspaceTools, contract::DEFAULT_TOOL_OUTPUT_TOKENS};

use super::{
    DynamicToolProvider, ImageGenerationConfig, Tool, ToolContext, ToolExposure, ToolInput,
    ToolOutput, ToolRuntime, Tools, WebSearchConfig,
};

struct Double;

struct Fails;

struct Panics;

struct PanickingProvider;

struct ReplacementExec;

struct Search {
    activated: Arc<AtomicBool>,
}

struct NativeSearch;

struct DeferredProvider {
    activated: Arc<AtomicBool>,
    started: AtomicBool,
}

struct ProviderStartState {
    started: AtomicBool,
    startups: AtomicUsize,
}

struct StartTrackingProvider {
    state: Arc<ProviderStartState>,
}

struct CollisionTool;

struct NamedTool {
    name: &'static str,
    output: &'static str,
}

struct DeclaredProvider {
    name: &'static str,
    parallel_safe: bool,
    output: &'static str,
}

#[derive(Deserialize)]
struct DoubleInput {
    value: i64,
}

#[async_trait::async_trait]
impl Tool for Double {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "double",
            "Doubles an integer.",
            json!({
                "type": "object",
                "properties": { "value": { "type": "integer" } },
                "required": ["value"],
                "additionalProperties": false
            }),
        )
    }

    async fn execute(&self, input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
        let input = input.decode_json::<DoubleInput>()?;
        Ok(ToolOutput::json(&(input.value * 2)))
    }
}

#[async_trait::async_trait]
impl Tool for NamedTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            self.name,
            format!("Returns {}.", self.output),
            json!({ "type": "object", "properties": {} }),
        )
    }

    async fn execute(&self, _input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
        Ok(ToolOutput::text(self.output))
    }
}

#[async_trait::async_trait]
impl Tool for Fails {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "fails",
            "Always fails.",
            json!({ "type": "object", "properties": {} }),
        )
    }

    async fn execute(&self, _input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
        Err(std::io::Error::other("intentional handler failure").into())
    }
}

#[async_trait::async_trait]
impl Tool for Panics {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "panics",
            "Panics for runtime-isolation tests.",
            json!({ "type": "object", "properties": {} }),
        )
    }

    async fn execute(&self, _input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
        panic!("registered handler panic payload")
    }
}

#[async_trait::async_trait]
impl DynamicToolProvider for PanickingProvider {
    fn start(&self) {}

    fn direct_tools(&self) -> Vec<Arc<dyn Tool>> {
        Vec::new()
    }

    fn available_definitions(&self) -> Vec<ToolDefinition> {
        vec![ToolDefinition::function(
            "provider_panic",
            "Panics for provider-isolation tests.",
            json!({ "type": "object", "properties": {} }),
        )]
    }

    async fn execute(
        &self,
        name: &str,
        _input: Value,
        _context: ToolContext<'_>,
    ) -> Option<ToolOutput> {
        assert_eq!(name, "provider_panic");
        panic!("dynamic provider panic payload")
    }
}

#[async_trait::async_trait]
impl Tool for ReplacementExec {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "exec_command",
            "Replacement command executor.",
            json!({
                "type": "object",
                "properties": { "cmd": { "type": "string" } },
                "required": ["cmd"],
                "additionalProperties": false
            }),
        )
    }

    async fn execute(&self, _input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
        Ok(ToolOutput::text("replacement"))
    }
}

#[async_trait::async_trait]
impl Tool for CollisionTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "collision",
            "Default-unsafe direct collision.",
            json!({ "type": "object", "properties": {} }),
        )
    }

    async fn execute(&self, _input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
        Ok(ToolOutput::text("direct"))
    }
}

#[async_trait::async_trait]
impl DynamicToolProvider for DeclaredProvider {
    fn start(&self) {}

    fn direct_tools(&self) -> Vec<Arc<dyn Tool>> {
        Vec::new()
    }

    fn available_definitions(&self) -> Vec<ToolDefinition> {
        vec![ToolDefinition::function(
            self.name,
            "Declared provider collision.",
            json!({ "type": "object", "properties": {} }),
        )]
    }

    fn supports_parallel_tool_calls(&self, name: &str) -> bool {
        name == self.name && self.parallel_safe
    }

    async fn execute(
        &self,
        name: &str,
        _input: serde_json::Value,
        _context: ToolContext<'_>,
    ) -> Option<ToolOutput> {
        (name == self.name).then(|| ToolOutput::text(self.output))
    }
}

#[async_trait::async_trait]
impl Tool for Search {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "tool_search",
            "Activates a matching deferred tool.",
            json!({
                "type": "object",
                "properties": { "query": { "type": "string" } },
                "required": ["query"],
                "additionalProperties": false
            }),
        )
    }

    async fn execute(&self, _input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
        self.activated.store(true, Ordering::Release);
        Ok(ToolOutput::from_json(
            json!({ "name": "deferred_echo" }),
            true,
        ))
    }
}

#[async_trait::async_trait]
impl Tool for NativeSearch {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::tool_search(
            "client",
            "Searches deferred tools.",
            JsonSchema::from(json!({
                "type": "object",
                "properties": { "query": { "type": "string" } },
                "required": ["query"],
                "additionalProperties": false
            })),
        )
    }

    async fn execute(&self, _input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
        Ok(ToolOutput::from_json(json!([]), true))
    }
}

#[async_trait::async_trait]
impl DynamicToolProvider for DeferredProvider {
    fn start(&self) {
        self.started.store(true, Ordering::Release);
    }

    fn direct_tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![Arc::new(Search {
            activated: Arc::clone(&self.activated),
        })]
    }

    fn available_definitions(&self) -> Vec<ToolDefinition> {
        self.activated
            .load(Ordering::Acquire)
            .then(|| {
                ToolDefinition::function(
                    "deferred_echo",
                    "Returns its input.",
                    json!({ "type": "object", "properties": {} }),
                )
            })
            .into_iter()
            .collect()
    }

    async fn execute(
        &self,
        name: &str,
        input: serde_json::Value,
        _context: ToolContext<'_>,
    ) -> Option<ToolOutput> {
        (name == "deferred_echo" && self.activated.load(Ordering::Acquire))
            .then(|| ToolOutput::from_json(input, true))
    }
}

#[async_trait::async_trait]
impl DynamicToolProvider for StartTrackingProvider {
    fn start(&self) {
        if !self.state.started.swap(true, Ordering::AcqRel) {
            self.state.startups.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn direct_tools(&self) -> Vec<Arc<dyn Tool>> {
        Vec::new()
    }

    fn available_definitions(&self) -> Vec<ToolDefinition> {
        Vec::new()
    }

    async fn execute(
        &self,
        _name: &str,
        _input: Value,
        _context: ToolContext<'_>,
    ) -> Option<ToolOutput> {
        None
    }
}

fn runtime(web_search: bool) -> ToolRuntime {
    ToolRuntime::new(
        ".",
        web_search.then(|| WebSearchConfig {
            endpoint: "http://127.0.0.1:1/v1/alpha/search".to_owned(),
            auth: OpenAiAuth::api_key("test-key"),
        }),
        Some(ImageGenerationConfig {
            api_base_url: "http://127.0.0.1:1/v1".to_owned(),
            auth: OpenAiAuth::api_key("test-key"),
            save_root: std::env::temp_dir().join("nanocodex-test-images"),
        }),
    )
}

#[test]
fn web_search_handler_and_spec_are_absent_when_disabled() {
    let enabled = runtime(true);
    assert!(
        enabled
            .registry
            .entries()
            .any(|(_, definition)| definition.name() == "web__run")
    );
    assert!(enabled.supports_parallel_tool_calls("web__run"));
    assert!(!enabled.supports_parallel_tool_calls("image_gen__imagegen"));
    let enabled_specs = serde_json::to_value(enabled.model_specs("test-session")).unwrap();
    assert!(
        enabled_specs[0]["description"]
            .as_str()
            .is_some_and(|description| description.contains("`web__run`"))
    );

    let disabled = runtime(false);
    assert!(
        disabled
            .registry
            .entries()
            .all(|(_, definition)| definition.name() != "web__run")
    );
    let disabled_specs = serde_json::to_value(disabled.model_specs("test-session")).unwrap();
    assert!(
        disabled_specs[0]["description"]
            .as_str()
            .is_some_and(|description| !description.contains("`web__run`"))
    );
}

#[test]
fn runtime_construction_starts_providers_and_preserves_eager_prewarm() {
    let standalone_state = Arc::new(ProviderStartState {
        started: AtomicBool::new(false),
        startups: AtomicUsize::new(0),
    });
    let standalone_tools = Tools::builder()
        .without_defaults()
        .provider(StartTrackingProvider {
            state: Arc::clone(&standalone_state),
        })
        .build()
        .unwrap();

    let _runtime = ToolRuntime::new_with_tools(".", None, None, &standalone_tools);
    assert!(standalone_state.started.load(Ordering::Acquire));
    assert_eq!(standalone_state.startups.load(Ordering::Relaxed), 1);

    let prewarmed_state = Arc::new(ProviderStartState {
        started: AtomicBool::new(false),
        startups: AtomicUsize::new(0),
    });
    let prewarmed_tools = Tools::builder()
        .without_defaults()
        .provider(StartTrackingProvider {
            state: Arc::clone(&prewarmed_state),
        })
        .build()
        .unwrap();

    prewarmed_tools.start_providers();
    let _runtime = ToolRuntime::new_with_tools(".", None, None, &prewarmed_tools);
    assert!(prewarmed_state.started.load(Ordering::Acquire));
    assert_eq!(prewarmed_state.startups.load(Ordering::Relaxed), 1);
}

#[test]
fn fixed_and_provider_catalog_collisions_are_rejected() {
    let direct_collision = Tools::builder()
        .without_defaults()
        .tool(CollisionTool)
        .provider(DeclaredProvider {
            name: "collision",
            parallel_safe: true,
            output: "provider",
        })
        .build();
    assert!(matches!(
        direct_collision,
        Err(super::ToolsBuildError::DuplicateName(name)) if name.as_ref() == "collision"
    ));

    let provider_collision = Tools::builder()
        .without_defaults()
        .provider(DeclaredProvider {
            name: "provider_collision",
            parallel_safe: false,
            output: "first",
        })
        .provider(DeclaredProvider {
            name: "provider_collision",
            parallel_safe: true,
            output: "second",
        })
        .build();
    assert!(matches!(
        provider_collision,
        Err(super::ToolsBuildError::DuplicateName(name))
            if name.as_ref() == "provider_collision"
    ));
}

#[test]
fn without_defaults_allows_replacing_a_standard_workspace_tool() {
    assert!(Tools::builder().tool(ReplacementExec).build().is_err());

    let tools = Tools::builder()
        .without_defaults()
        .tool(ReplacementExec)
        .build()
        .unwrap();
    let runtime = ToolRuntime::new_with_tools(".", None, None, &tools);
    let names = runtime
        .registry
        .entries()
        .map(|(_, definition)| definition.name())
        .collect::<Vec<_>>();

    assert_eq!(names, ["exec_command"]);
}

#[test]
fn workspace_tool_source_is_a_singleton() {
    let result = Tools::builder()
        .without_defaults()
        .add(WorkspaceTools::new("first"))
        .add(WorkspaceTools::new("second"))
        .build();

    assert!(matches!(
        result,
        Err(super::ToolsBuildError::DuplicateSource("workspace"))
    ));
}

#[tokio::test]
async fn workspace_tool_source_overrides_the_runtime_root_and_retains_shell_sessions() {
    let source_workspace = tempdir().unwrap();
    let ignored_workspace = tempdir().unwrap();
    let tools = Tools::builder()
        .without_defaults()
        .add(WorkspaceTools::new(source_workspace.path()))
        .build()
        .unwrap();
    let runtime = ToolRuntime::new_with_tools(ignored_workspace.path(), None, None, &tools);

    assert_eq!(
        runtime.working_directory(),
        source_workspace.path().to_str().unwrap()
    );

    let output = runtime
        .execute_tool(
            "exec_command",
            ToolInput::Function(
                to_raw_value(&json!({
                    "cmd": "pwd; sleep 30",
                    "yield_time_ms": 250,
                }))
                .unwrap(),
            ),
            ToolContext::new(
                "test-model",
                "test-session",
                "test-call",
                &[],
                DEFAULT_TOOL_OUTPUT_TOKENS,
            ),
        )
        .await;
    assert!(output.success);
    assert!(
        output.structured_result()["output"]
            .as_str()
            .is_some_and(|stdout| stdout.contains(source_workspace.path().to_str().unwrap()))
    );
    let session_id = output
        .process_trace()
        .and_then(|process| process.session_id)
        .expect("long-running workspace command should retain a shell session");
    assert!(runtime.has_shell_session(session_id).await);

    runtime.control().cancel().await;
}

#[test]
fn model_description_is_stable_across_registration_order() {
    let first = Tools::builder()
        .without_defaults()
        .tool(Fails)
        .tool(Double)
        .build()
        .unwrap();
    let second = Tools::builder()
        .without_defaults()
        .tool(Double)
        .tool(Fails)
        .build()
        .unwrap();

    let first = serde_json::to_vec(
        &ToolRuntime::new_with_tools(".", None, None, &first).model_specs("test-session"),
    )
    .unwrap();
    let second = serde_json::to_vec(
        &ToolRuntime::new_with_tools(".", None, None, &second).model_specs("test-session"),
    )
    .unwrap();

    assert_eq!(first, second);
}

#[test]
fn exposure_controls_direct_visibility_without_removing_code_mode_access() {
    let code_mode_only = Tools::builder()
        .without_defaults()
        .tool(Double)
        .build()
        .unwrap();
    assert_eq!(code_mode_only.exposure(), ToolExposure::CodeModeOnly);
    let code_mode_only = ToolRuntime::new_with_tools(".", None, None, &code_mode_only);
    let code_mode_only_contract = code_mode_only.model_contract("test-session").1;
    assert_eq!(
        code_mode_only
            .model_specs("test-session")
            .iter()
            .map(ToolDefinition::name)
            .collect::<Vec<_>>(),
        ["exec", "wait"]
    );

    let code_mode = Tools::builder()
        .without_defaults()
        .exposure(ToolExposure::DirectAndCodeMode)
        .tool(Double)
        .build()
        .unwrap();
    assert_eq!(code_mode.exposure(), ToolExposure::DirectAndCodeMode);
    let code_mode = ToolRuntime::new_with_tools(".", None, None, &code_mode);
    let code_mode_contract = code_mode.model_contract("test-session").1;
    let specs = code_mode.model_specs("test-session");
    assert_eq!(
        specs.iter().map(ToolDefinition::name).collect::<Vec<_>>(),
        ["exec", "wait", "double"]
    );
    let exec = specs
        .iter()
        .find(|definition| definition.name() == "exec")
        .unwrap();
    assert!(
        !serde_json::to_value(exec).unwrap()["description"]
            .as_str()
            .is_some_and(|description| description.contains(
                "declare const tools: { double(args: { value: number; }): Promise<unknown>; };"
            )),
        "normal Code Mode keeps exec terse like Codex"
    );
    let direct_double = specs
        .iter()
        .find(|definition| definition.name() == "double")
        .unwrap();
    assert!(
        direct_double.description().contains(
            "declare const tools: { double(args: { value: number; }): Promise<unknown>; };"
        ),
        "normal Code Mode augments each direct spec with its exec declaration"
    );
    assert_eq!(code_mode_contract, code_mode_only_contract);
}

#[test]
fn per_tool_exposure_selects_direct_and_code_mode_surfaces_independently() {
    let tools = Tools::builder()
        .without_defaults()
        .tool(NamedTool {
            name: "nested_only",
            output: "nested",
        })
        .tool_with_exposure(
            NamedTool {
                name: "direct_only",
                output: "direct",
            },
            ToolExposure::DirectOnly,
        )
        .tool_with_exposure(
            NamedTool {
                name: "hidden",
                output: "hidden",
            },
            ToolExposure::Hidden,
        )
        .build()
        .unwrap();
    let runtime = ToolRuntime::new_with_tools(".", None, None, &tools);

    assert_eq!(
        runtime
            .model_specs("test-session")
            .iter()
            .map(ToolDefinition::name)
            .collect::<Vec<_>>(),
        ["exec", "wait", "direct_only"]
    );
    assert_eq!(
        runtime.model_contract("test-session").1,
        [("nested_only".to_owned(), "nested_only".to_owned())]
    );
    assert!(
        runtime.contains("hidden"),
        "hidden tools remain dispatchable"
    );
}

#[test]
fn registered_normalized_code_mode_name_collisions_are_rejected() {
    let result = Tools::builder()
        .without_defaults()
        .exposure(ToolExposure::DirectAndCodeMode)
        .tool(NamedTool {
            name: "normalized-alias",
            output: "first",
        })
        .tool(NamedTool {
            name: "normalized_alias",
            output: "second",
        })
        .build();

    assert!(matches!(
        result,
        Err(super::ToolsBuildError::NormalizedNameCollision {
            first,
            second,
            normalized,
        }) if first.as_ref() == "normalized-alias"
            && second.as_ref() == "normalized_alias"
            && normalized.as_ref() == "normalized_alias"
    ));
}

#[test]
fn registered_public_tool_names_match_the_wire_grammar() {
    let too_long: &'static str = Box::leak("a".repeat(129).into_boxed_str());
    for name in ["_starts_wrong", "has space", "unicodé", too_long] {
        let result = Tools::builder()
            .without_defaults()
            .tool(NamedTool {
                name,
                output: "invalid",
            })
            .build();
        assert!(matches!(
            result,
            Err(super::ToolsBuildError::InvalidPublicName(candidate)) if candidate.as_ref() == name
        ));
    }

    assert!(
        Tools::builder()
            .without_defaults()
            .tool(NamedTool {
                name: "a.valid:tool-name_1",
                output: "valid",
            })
            .build()
            .is_ok()
    );
}

#[test]
fn published_catalog_names_reject_invalid_and_normalized_collisions() {
    assert!(matches!(
        crate::selection::validate_public_tool_catalog_names(["invalid name"]),
        Err(crate::selection::PublicToolCatalogError::InvalidName(name))
            if name.as_ref() == "invalid name"
    ));
    assert!(matches!(
        crate::selection::validate_public_tool_catalog_names([
            "mcp__docs__read-file",
            "mcp__docs__read_file",
        ]),
        Err(crate::selection::PublicToolCatalogError::NormalizedNameCollision {
            first,
            second,
            normalized,
        }) if first.as_ref() == "mcp__docs__read-file"
            && second.as_ref() == "mcp__docs__read_file"
            && normalized.as_ref() == "mcp__docs__read_file"
    ));
}

#[test]
fn registered_tools_cannot_replace_host_owned_routing_tools() {
    for name in ["exec", "wait", "tool_search"] {
        let result = Tools::builder()
            .without_defaults()
            .tool(NamedTool {
                name,
                output: "replacement",
            })
            .build();
        assert!(matches!(
            result,
            Err(super::ToolsBuildError::ReservedName(candidate)) if candidate.as_ref() == name
        ));
    }

    assert!(
        Tools::builder()
            .without_defaults()
            .tool(NativeSearch)
            .build()
            .is_ok()
    );
}

#[test]
fn composing_a_recipe_revalidates_normalized_names() {
    let tools = Tools::builder()
        .without_defaults()
        .tool(NamedTool {
            name: "read-file",
            output: "first",
        })
        .build()
        .unwrap();
    let result = tools
        .into_builder()
        .tool(NamedTool {
            name: "read_file",
            output: "second",
        })
        .build();

    assert!(matches!(
        result,
        Err(super::ToolsBuildError::NormalizedNameCollision { normalized, .. })
            if normalized.as_ref() == "read_file"
    ));
}

#[tokio::test]
async fn hidden_private_names_remain_dispatchable() {
    let tools = Tools::builder()
        .without_defaults()
        .tool_with_exposure(
            NamedTool {
                name: "_internal/tool",
                output: "private",
            },
            ToolExposure::Hidden,
        )
        .build()
        .unwrap();
    let runtime = ToolRuntime::new_with_tools(".", None, None, &tools);
    let output = runtime
        .execute_tool(
            "_internal/tool",
            ToolInput::Function(to_raw_value(&json!({})).unwrap()),
            ToolContext::new(
                "test-model",
                "test-session",
                "test-call",
                &[],
                DEFAULT_TOOL_OUTPUT_TOKENS,
            ),
        )
        .await;

    assert_eq!(output.structured_result(), json!("private"));
    assert!(
        runtime
            .model_specs("test-session")
            .iter()
            .all(|definition| {
                !serde_json::to_string(definition)
                    .unwrap()
                    .contains("_internal/tool")
            })
    );
    assert!(matches!(
        Tools::builder()
            .without_defaults()
            .tool(NamedTool {
                name: "_internal/tool",
                output: "visible",
            })
            .build(),
        Err(super::ToolsBuildError::InvalidPublicName(name))
            if name.as_ref() == "_internal/tool"
    ));
}

#[test]
fn tool_recipe_overrides_model_visible_environment_context() {
    let tools = Tools::builder()
        .without_defaults()
        .working_directory("/workspace")
        .default_shell("sh")
        .build()
        .unwrap();
    let runtime = ToolRuntime::new_with_tools("/host/attempt", None, None, &tools);

    assert_eq!(runtime.working_directory(), "/workspace");
    assert_eq!(runtime.default_shell_name(), "sh");
}

#[test]
fn session_binding_overrides_only_its_process_environment_clone() {
    let tools = Tools::builder()
        .process_environment([
            ("OTHER_VARIABLE", "preserved"),
            ("CODEX_THREAD_ID", "caller-spoof"),
        ])
        .build()
        .unwrap();
    let bound = tools.clone().for_session("session-1");

    assert_eq!(
        tools.process_environment().as_slice(),
        [
            (
                OsString::from("OTHER_VARIABLE"),
                OsString::from("preserved")
            ),
            (
                OsString::from("CODEX_THREAD_ID"),
                OsString::from("caller-spoof")
            ),
        ]
    );
    assert_eq!(
        bound.process_environment().as_slice(),
        [
            (
                OsString::from("OTHER_VARIABLE"),
                OsString::from("preserved")
            ),
            (
                OsString::from("CODEX_THREAD_ID"),
                OsString::from("session-1")
            ),
        ]
    );
}

#[test]
fn tool_recipe_rejects_empty_environment_overrides() {
    assert!(matches!(
        Tools::builder().working_directory(" ").build(),
        Err(super::ToolsBuildError::EmptyWorkingDirectory)
    ));
    assert!(matches!(
        Tools::builder().default_shell("").build(),
        Err(super::ToolsBuildError::EmptyDefaultShell)
    ));
}

#[tokio::test]
async fn registered_tool_is_described_and_callable_from_code_mode() {
    let tools = Tools::builder()
        .without_defaults()
        .tool(Double)
        .build()
        .unwrap();
    let runtime = ToolRuntime::new_with_tools(".", None, None, &tools);
    let description = serde_json::to_value(runtime.model_specs("test-session")).unwrap();
    assert!(
        description[0]["description"]
            .as_str()
            .is_some_and(|description| description.contains(
                "declare const tools: { double(args: { value: number; }): Promise<unknown>; };"
            ))
    );

    let execution = runtime
        .execute_code(
            r"
const result = await tools.double({ value: 21 });
text(result);
",
            ToolContext::new(
                "test-model",
                "test-session",
                "test-call",
                &[],
                DEFAULT_TOOL_OUTPUT_TOKENS,
            ),
        )
        .await;
    assert!(execution.success);
    assert_eq!(execution.nested_calls.len(), 1);
    assert_eq!(execution.nested_calls[0].name, "double");
    assert_eq!(execution.nested_calls[0].input, json!({ "value": 21 }));
    assert_eq!(execution.nested_calls[0].structured_result, json!(42));
    let ToolOutputBody::Content(content) = execution.output else {
        panic!("expected content output");
    };
    assert_eq!(
        serde_json::to_value(content)
            .unwrap()
            .as_array()
            .unwrap()
            .last(),
        Some(&json!({ "type": "input_text", "text": "42" }))
    );
}

#[tokio::test]
async fn handler_errors_become_failed_model_visible_results() {
    let tools = Tools::builder()
        .without_defaults()
        .tool(Fails)
        .build()
        .unwrap();
    let runtime = ToolRuntime::new_with_tools(".", None, None, &tools);
    let execution = runtime
        .registry
        .execute_nested(
            "fails",
            json!({}),
            ToolContext::new(
                "test-model",
                "test-session",
                "test-call",
                &[],
                DEFAULT_TOOL_OUTPUT_TOKENS,
            ),
        )
        .await;

    assert!(!execution.success);
    assert!(matches!(
        execution.output,
        ToolOutputBody::Text(output) if output == "intentional handler failure"
    ));
}

#[tokio::test]
async fn handler_panics_become_aborted_outputs_without_escaping_the_runtime() {
    let tools = Tools::builder()
        .without_defaults()
        .tool(Panics)
        .provider(PanickingProvider)
        .build()
        .unwrap();
    let runtime = ToolRuntime::new_with_tools(".", None, None, &tools);
    let context = ToolContext::new(
        "test-model",
        "test-session",
        "test-call",
        &[],
        DEFAULT_TOOL_OUTPUT_TOKENS,
    );

    let registered = runtime
        .registry
        .execute_nested("panics", json!({}), context)
        .await;
    assert!(!registered.success);
    assert!(matches!(
        registered.output,
        ToolOutputBody::Text(output) if output == "aborted"
    ));

    let provider = runtime
        .execute_tool(
            "provider_panic",
            ToolInput::Function(to_raw_value(&json!({})).unwrap()),
            context,
        )
        .await;
    assert!(!provider.success);
    assert!(matches!(
        provider.output,
        ToolOutputBody::Text(output) if output == "aborted"
    ));
}

#[tokio::test]
async fn direct_model_calls_reach_activated_dynamic_tools() {
    let tools = Tools::builder()
        .without_defaults()
        .provider(DeferredProvider {
            activated: Arc::new(AtomicBool::new(false)),
            started: AtomicBool::new(false),
        })
        .build()
        .unwrap();
    tools.start_providers();
    let runtime = ToolRuntime::new_with_tools(".", None, None, &tools);
    let context = ToolContext::new(
        "test-model",
        "test-session",
        "test-call",
        &[],
        DEFAULT_TOOL_OUTPUT_TOKENS,
    );

    let search = runtime
        .execute_tool(
            "tool_search",
            ToolInput::Function(to_raw_value(&json!({ "query": "echo" })).unwrap()),
            context,
        )
        .await;
    assert!(search.success);

    let execution = runtime
        .execute_tool(
            "deferred_echo",
            ToolInput::Function(to_raw_value(&json!({ "value": 21 })).unwrap()),
            context,
        )
        .await;
    assert!(execution.success);
    assert_eq!(execution.structured_result(), json!({ "value": 21 }));
}

#[tokio::test]
async fn code_mode_can_search_and_call_a_deferred_tool_in_one_cell() {
    let tools = Tools::builder()
        .without_defaults()
        .provider(DeferredProvider {
            activated: Arc::new(AtomicBool::new(false)),
            started: AtomicBool::new(false),
        })
        .build()
        .unwrap();
    tools.start_providers();
    let runtime = ToolRuntime::new_with_tools(".", None, None, &tools);
    let model_specs_before = serde_json::to_vec(&runtime.model_specs("test-session")).unwrap();
    let model_specs_value = serde_json::to_value(runtime.model_specs("test-session")).unwrap();
    assert!(
        model_specs_value[0]["description"]
            .as_str()
            .is_some_and(|description| !description.contains("Shared MCP Types:")),
        "ordinary deferred providers must not opt into MCP-specific guidance"
    );
    let execution = runtime
        .execute_code(
            r#"
const found = await tools.tool_search({ query: "echo" });
const result = await tools[found.name]({ value: 21 });
text(result.value);
"#,
            ToolContext::new(
                "test-model",
                "test-session",
                "test-call",
                &[],
                DEFAULT_TOOL_OUTPUT_TOKENS,
            ),
        )
        .await;

    assert!(execution.success);
    assert_eq!(
        serde_json::to_vec(&runtime.model_specs("test-session")).unwrap(),
        model_specs_before,
        "activating deferred tools must not change the model request prefix"
    );
    assert_eq!(execution.nested_calls.len(), 2);
    assert_eq!(execution.nested_calls[0].name, "tool_search");
    assert_eq!(execution.nested_calls[1].name, "deferred_echo");
    let ToolOutputBody::Content(content) = execution.output else {
        panic!("expected content output");
    };
    assert_eq!(
        serde_json::to_value(content)
            .unwrap()
            .as_array()
            .unwrap()
            .last(),
        Some(&json!({ "type": "input_text", "text": "21" }))
    );
}
