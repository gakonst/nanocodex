use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use nanocodex_oai_tools::{
    Tool, ToolContext, ToolDefinition, ToolInput, ToolOutput, ToolResult,
    contract::async_trait,
    runtime::{ToolExposure, ToolRuntime, Tools, ToolsBuilder},
};
use serde_json::{Value, json};

const REPORT: &str = "ledger.export-report";
const SCHEMA_NOTE: &str = "Complete synthetic schema detail. ";

struct LedgerTool {
    definition: ToolDefinition,
    effects: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for LedgerTool {
    fn definition(&self) -> ToolDefinition {
        self.definition.clone()
    }

    async fn execute(&self, input: ToolInput, context: ToolContext<'_>) -> ToolResult {
        if context.host_context() != Some("ledger:read") {
            return Ok(ToolOutput::error(
                "AUTHORIZATION_DENIED: ledger:read required",
            ));
        }
        let input = input.decode_json::<Value>()?;
        self.effects.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput::json(
            &json!({"account": input["account"], "rows": 7}),
        ))
    }
}

fn tool(name: &str, description: String, effects: &Arc<AtomicUsize>) -> LedgerTool {
    LedgerTool {
        definition: ToolDefinition::function(
            name,
            description,
            json!({
                "type": "object", "additionalProperties": false,
                "required": ["account"],
                "properties": {"account": {"type": "string", "enum": ["fixture-account"]}}
            }),
        ),
        effects: effects.clone(),
    }
}

fn catalog(effects: &Arc<AtomicUsize>) -> ToolsBuilder {
    let mut report = tool(
        REPORT,
        "Export synthetic ledger report. ".repeat(800),
        effects,
    );
    report.definition = ToolDefinition::function(
        REPORT,
        report.definition.description(),
        json!({
            "type": "object", "additionalProperties": false,
            "required": ["account"],
            "properties": {"account": {
                "type": "string", "enum": ["fixture-account"],
                "description": SCHEMA_NOTE.repeat(800)
            }}
        }),
    )
    .with_output_schema(json!({
        "type": "object", "required": ["account", "rows"],
        "properties": {"account": {"type": "string"}, "rows": {"type": "integer", "minimum": 0}}
    }));
    let mut builder = Tools::builder().without_defaults().tool(report).tool(tool(
        "ledger__lookup",
        "Look up a synthetic ledger account.".into(),
        effects,
    ));
    // Enough documentation to exercise the default budget, even after the
    // oversized report declaration has been omitted as an indivisible entry.
    for index in 0..40 {
        builder = builder.tool(tool(
            &format!("ledger__detail_{index}"),
            "Synthetic ledger detail documentation. ".repeat(20),
            effects,
        ));
    }
    builder
}

fn context(authorized: bool) -> ToolContext<'static> {
    ToolContext::new(
        "fixture-model",
        "discovery-session",
        "discovery-exec",
        &[],
        10_000,
    )
    .with_host_context(authorized.then_some("ledger:read"))
}

fn exec_description(runtime: &ToolRuntime) -> String {
    runtime
        .model_specs("discovery-session")
        .into_iter()
        .find(|definition| definition.name() == "exec")
        .unwrap()
        .description()
        .to_owned()
}

// Public embedding journey: the real native QuickJS evaluator receives the
// admitted catalog, while the model sees only the bounded exec documentation.
#[tokio::test]
async fn code_discovery_omitted_docs_preserve_full_schemas_and_invocation() {
    let _serial = super::TOOL_RUNTIME_TEST_LOCK.lock().await;
    let workspace = tempfile::tempdir().unwrap();
    let effects = Arc::new(AtomicUsize::new(0));
    let zero = catalog(&effects)
        .inline_docs_token_budget(0)
        .build()
        .unwrap();
    let zero_runtime = ToolRuntime::new_with_tools(workspace.path(), None, None, &zero);
    let baseline = exec_description(&zero_runtime);
    let defaults = catalog(&effects).build().unwrap();
    let default_runtime = ToolRuntime::new_with_tools(workspace.path(), None, None, &defaults);
    let default_docs = exec_description(&default_runtime);

    for budget in [0, 96, 3000] {
        let tools = catalog(&effects)
            .inline_docs_token_budget(budget)
            .build()
            .unwrap();
        let runtime = ToolRuntime::new_with_tools(workspace.path(), None, None, &tools);
        let docs = exec_description(&runtime);
        let inline_bytes = docs.len().checked_sub(baseline.len()).unwrap();
        assert!(
            inline_bytes <= budget * 4,
            "budget={budget} inline_bytes={inline_bytes}"
        );
        if budget > 0 {
            assert!(inline_bytes > 0);
        }
        if budget == 3000 {
            assert_eq!(docs, default_docs);
        }
        assert!(
            !docs.contains(REPORT),
            "oversized declaration must be omitted"
        );
        assert!(
            !docs.contains(SCHEMA_NOTE),
            "schema must remain outside inline docs"
        );

        let source = r#"
            const check = (ok, label) => { if (!ok) throw new Error(label); };
            const matches = searchTools("EXPORT synthetic", {limit: 1});
            check(matches.length === 1 && matches[0].name === "ledger.export-report", "search result");
            const description = describeTool(matches[0].callableName);
            check(description.callableName === "ledger_export_report", "normalized callable");
            check(describeTool(matches[0].name) === description, "canonical lookup");
            check(description.inputSchema.properties.account.description === "Complete synthetic schema detail. ".repeat(800), "complete schema");
            check(description.inputSchema.required[0] === "account", "required argument");
            check(description.outputSchema.properties.rows.minimum === 0, "output schema");
            check(describeNamespace("ledger").length === 42, "namespace catalog");
            check(ALL_TOOLS.length === 42, "compatibility catalog");
            check(toolSchema(description.callableName).inputSchema.properties.account.enum[0] === "fixture-account", "compatibility schema");
            check(Object.isFrozen(description) && Object.isFrozen(description.inputSchema.properties.account), "immutable discovery");
            description.inputSchema.properties.account.enum[0] = "forged-account";
            check(describeTool(description.name).inputSchema.properties.account.enum[0] === "fixture-account", "schema remains immutable");
            const receipt = await tools[description.callableName]({account: "fixture-account"});
            check(receipt.rows === 7 && receipt.account === "fixture-account", "invocation result");
            text({name: description.name, callableName: description.callableName,
                schemaBytes: description.inputSchema.properties.account.description.length, receipt});
        "#;
        let execution = runtime.execute_code(source, context(true)).await.unwrap();
        eprintln!(
            "code_discovery budget={budget} inline_bytes={inline_bytes} max_bytes={} output={:?} nested={:?}",
            budget * 4,
            execution.output,
            execution.nested_calls
        );
        assert!(execution.success, "{:?}", execution.output);
        assert!(!execution.cell.as_ref().unwrap().running);
        assert_eq!(execution.nested_calls.len(), 1);
        let call = &execution.nested_calls[0];
        assert_eq!(call.name, REPORT);
        assert_eq!(call.input, json!({"account": "fixture-account"}));
        assert!(call.success);
        assert_eq!(
            call.structured_result,
            json!({"account": "fixture-account", "rows": 7})
        );
        runtime.control().cancel().await;
    }
    assert_eq!(effects.load(Ordering::SeqCst), 3);
    zero_runtime.control().cancel().await;
    default_runtime.control().cancel().await;
}

#[tokio::test]
async fn code_discovery_denials_do_not_dispatch_unknown_or_excluded_tools() {
    let _serial = super::TOOL_RUNTIME_TEST_LOCK.lock().await;
    let workspace = tempfile::tempdir().unwrap();
    let effects = Arc::new(AtomicUsize::new(0));
    let tools = Tools::builder()
        .without_defaults()
        .inline_docs_token_budget(0)
        .tool(tool(
            "ledger__lookup",
            "Read a synthetic ledger account.".into(),
            &effects,
        ))
        .tool_with_exposure(
            tool("direct_read", "Direct read.".into(), &effects),
            ToolExposure::DirectOnly,
        )
        .tool_with_exposure(
            tool("hidden_read", "Hidden read.".into(), &effects),
            ToolExposure::Hidden,
        )
        .build()
        .unwrap();
    let runtime = ToolRuntime::new_with_tools(workspace.path(), None, None, &tools);
    let source = r#"
        const check = (ok, label) => { if (!ok) throw new Error(label); };
        check(searchTools("read").length === 1, "only admitted tools discoverable");
        const unavailable = [];
        for (const name of ["unknown_read", "direct_read", "hidden_read"]) {
            check(describeTool(name) === undefined, "excluded description " + name);
            check(!ALL_TOOLS.some(tool => tool.name === name), "excluded catalog " + name);
            try { await tools[name]({account: "fixture-account"}); throw new Error("unexpected dispatch"); }
            catch (error) {
                check(error.code === "TOOL_NOT_AVAILABLE" && error.tool === name, "local rejection " + name);
                unavailable.push({name, code: error.code});
            }
        }
        const admitted = describeTool(searchTools("synthetic ledger")[0].callableName);
        let denial;
        try { await tools[admitted.callableName]({account: "fixture-account"}); throw new Error("unexpected authorization"); }
        catch (error) { denial = error; }
        check(denial === "AUTHORIZATION_DENIED: ledger:read required", "authorization failure");
        text({unavailable, denial});
    "#;
    let denied = runtime.execute_code(source, context(false)).await.unwrap();
    eprintln!(
        "code_discovery denied output={:?} nested={:?} effects={}",
        denied.output,
        denied.nested_calls,
        effects.load(Ordering::SeqCst)
    );
    assert!(denied.success, "{:?}", denied.output);
    assert_eq!(
        denied.nested_calls.len(),
        1,
        "missing/excluded names must not dispatch"
    );
    assert_eq!(denied.nested_calls[0].name, "ledger__lookup");
    assert!(!denied.nested_calls[0].success);
    assert_eq!(
        denied.nested_calls[0].structured_result,
        json!("AUTHORIZATION_DENIED: ledger:read required")
    );
    assert_eq!(effects.load(Ordering::SeqCst), 0);

    // The denial must not poison the runtime: a later host-authorized cell can
    // discover and call the same handler with its original schema.
    let recovered = runtime.execute_code(r#"
        const tool = describeTool(searchTools("synthetic ledger")[0].name);
        text(await tools[tool.callableName]({account: tool.inputSchema.properties.account.enum[0]}));
    "#, context(true)).await.unwrap();
    eprintln!(
        "code_discovery recovery output={:?} nested={:?} effects={}",
        recovered.output,
        recovered.nested_calls,
        effects.load(Ordering::SeqCst)
    );
    assert!(recovered.success, "{:?}", recovered.output);
    assert_eq!(recovered.nested_calls.len(), 1);
    assert!(recovered.nested_calls[0].success);
    assert_eq!(
        recovered.nested_calls[0].structured_result,
        json!({"account": "fixture-account", "rows": 7})
    );
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    runtime.control().cancel().await;
}

#[tokio::test]
async fn code_discovery_callable_matches_native_dispatch() {
    let _serial = super::TOOL_RUNTIME_TEST_LOCK.lock().await;
    let workspace = tempfile::tempdir().unwrap();
    let effects = Arc::new(AtomicUsize::new(0));
    let tools = Tools::builder()
        .without_defaults()
        .inline_docs_token_budget(0)
        .tool(tool(
            "3ledger.export-report",
            "Punctuation named lookup.".into(),
            &effects,
        ))
        .build()
        .unwrap();
    let runtime = ToolRuntime::new_with_tools(workspace.path(), None, None, &tools);
    let execution = runtime
        .execute_code(
            r#"
        const found = searchTools('Punctuation')[0];
        const full = describeTool(found.name);
        if (full.callableName !== '_ledger_export_report') throw new Error(full.callableName);
        text({name: full.name, callableName: full.callableName,
          receipt: await tools[full.callableName]({account: 'fixture-account'})});
    "#,
            context(true),
        )
        .await
        .unwrap();
    eprintln!(
        "code_discovery normalized output={:?} nested={:?}",
        execution.output, execution.nested_calls
    );
    assert!(execution.success, "{:?}", execution.output);
    assert_eq!(execution.nested_calls.len(), 1);
    assert_eq!(execution.nested_calls[0].name, "3ledger.export-report");
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    runtime.control().cancel().await;
}
