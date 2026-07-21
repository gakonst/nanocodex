use std::sync::{Arc, Mutex};

use nanocodex_tools::{ToolContext, ToolRuntime, Tools};
use tracing::{
    Event, Instrument, Subscriber,
    field::{Field, Visit},
    span::Attributes,
};
use tracing_subscriber::{Layer, layer::Context as LayerContext, prelude::*, registry::LookupSpan};

#[derive(Clone)]
struct ToolTraceCapture {
    has_tool_call_ancestor: Arc<Mutex<bool>>,
    contents: Arc<Mutex<Vec<(String, String)>>>,
}

#[derive(Default)]
struct ContentVisitor {
    kind: Option<String>,
    content: Option<String>,
}

impl Visit for ContentVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "content_kind" => self.kind = Some(value.to_owned()),
            "content" => self.content = Some(value.to_owned()),
            _ => {}
        }
    }

    fn record_debug(&mut self, _field: &Field, _value: &dyn std::fmt::Debug) {}
}

impl<S> Layer<S> for ToolTraceCapture
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
        *self.has_tool_call_ancestor.lock().unwrap() = has_tool_call_ancestor;
    }

    fn on_event(&self, event: &Event<'_>, context: LayerContext<'_, S>) {
        let Some(span) = context.event_span(event) else {
            return;
        };
        if !span
            .scope()
            .any(|ancestor| ancestor.metadata().name() == "tool.execute")
        {
            return;
        }
        let mut visitor = ContentVisitor::default();
        event.record(&mut visitor);
        if let (Some(kind), Some(content)) = (visitor.kind, visitor.content) {
            self.contents.lock().unwrap().push((kind, content));
        }
    }
}

#[test]
fn spawned_cell_and_direct_tool_preserve_the_parent_span() {
    let has_tool_call_ancestor = Arc::new(Mutex::new(false));
    let contents = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::registry().with(ToolTraceCapture {
        has_tool_call_ancestor: Arc::clone(&has_tool_call_ancestor),
        contents: Arc::clone(&contents),
    });
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
    std::fs::write(workspace.join("notes.txt"), "alpha\n").unwrap();
    let selected = Tools::builder().build().unwrap();
    let tools = ToolRuntime::new(&workspace, None, None).with_tools(&selected);
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

    *has_tool_call_ancestor.lock().unwrap() = false;
    let direct = tracing::dispatcher::with_default(&dispatch, || {
        runtime.block_on(
            tools
                .execute_direct("hashline__read", r#"{"path":"notes.txt"}"#, context)
                .instrument(tracing::info_span!("test.tool_call")),
        )
    });
    assert!(direct.success);
    assert!(*has_tool_call_ancestor.lock().unwrap());
    let expected_output = serde_json::to_string(&direct.output).unwrap();
    let contents = contents.lock().unwrap();
    assert!(contents.iter().any(|(kind, content)| {
        kind == "tool.arguments" && content == r#"{"path":"notes.txt"}"#
    }));
    assert!(
        contents
            .iter()
            .any(|(kind, content)| kind == "tool.output" && content == &expected_output)
    );
    std::fs::remove_dir_all(workspace).unwrap();
}
