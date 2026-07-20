use std::sync::{Arc, Mutex};

use nanocodex_tools::{ToolContext, ToolRuntime};
use tracing::{Instrument, Subscriber, span::Attributes};
use tracing_subscriber::{Layer, layer::Context as LayerContext, prelude::*, registry::LookupSpan};

#[derive(Clone)]
struct ToolSpanAncestor(Arc<Mutex<bool>>);

impl<S> Layer<S> for ToolSpanAncestor
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    fn on_new_span(
        &self,
        attributes: &Attributes<'_>,
        _id: &tracing::Id,
        context: LayerContext<'_, S>,
    ) {
        if attributes.metadata().name() != "tool.execute" {
            return;
        }
        let parent_id = attributes.parent().cloned().or_else(|| {
            attributes
                .is_contextual()
                .then(|| context.current_span().id().cloned())
                .flatten()
        });
        let has_tool_call_ancestor = parent_id
            .as_ref()
            .and_then(|id| context.span(id))
            .is_some_and(|span| {
                span.scope()
                    .any(|ancestor| ancestor.metadata().name() == "test.tool_call")
            });
        *self.0.lock().unwrap() = has_tool_call_ancestor;
    }
}

#[test]
fn spawned_cell_preserves_the_parent_span_for_nested_tools() {
    let has_tool_call_ancestor = Arc::new(Mutex::new(false));
    let subscriber =
        tracing_subscriber::registry().with(ToolSpanAncestor(Arc::clone(&has_tool_call_ancestor)));
    let dispatch = tracing::Dispatch::new(subscriber);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let workspace = std::env::temp_dir().join(format!(
        "nanocodex-code-mode-trace-parent-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&workspace).unwrap();
    let tools = ToolRuntime::new(&workspace, None, None);
    let history = Vec::new();
    let context = ToolContext {
        model: "test-model",
        session_id: "test-session",
        call_id: "test-call",
        history: &history,
        output_token_budget: nanocodex_tools::DEFAULT_TOOL_OUTPUT_TOKENS,
    };

    let execution = tracing::dispatcher::with_default(&dispatch, || {
        runtime.block_on(
            tools
                .execute_code(
                    r#"await tools.exec_command({ cmd: "pwd", login: false });"#,
                    context,
                )
                .instrument(tracing::info_span!("test.tool_call")),
        )
    });

    assert!(execution.success);
    assert!(*has_tool_call_ancestor.lock().unwrap());
    std::fs::remove_dir_all(workspace).unwrap();
}
