use criterion::{Criterion, criterion_group, criterion_main};
use nanocodex_tools::{
    ToolContext, ToolInput, Tools, contract::DEFAULT_TOOL_OUTPUT_TOKENS, runtime::ToolRuntime, tool,
};
use serde_json::{json, value::to_raw_value};

#[tool(description = "Return the supplied benchmark message.")]
async fn benchmark_echo(message: String) -> Result<String, std::io::Error> {
    Ok(message)
}

fn benchmark_process_output(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("benchmark Tokio runtime must initialize");
    let tools = ToolRuntime::new(".", None, None);
    let input = to_raw_value(&json!({
        "cmd": "printf '%065536d' 0",
        "login": false,
        "max_output_tokens": 1_024
    }))
    .expect("benchmark input must serialize");
    let context = ToolContext::new(
        "benchmark-model",
        "benchmark-session",
        "benchmark-process",
        &[],
        DEFAULT_TOOL_OUTPUT_TOKENS,
    );

    c.bench_function("tool_process_output/64k_to_1k_tokens", |benchmark| {
        benchmark.to_async(&runtime).iter(|| async {
            let output = tools
                .execute_tool("exec_command", ToolInput::Function(input.clone()), context)
                .await;
            assert!(output.success);
            std::hint::black_box(output);
        });
    });

    let selected = Tools::builder()
        .without_defaults()
        .tool(benchmark_echo)
        .build()
        .expect("benchmark tool registry must be valid");
    let registered = ToolRuntime::new_with_tools(".", None, None, &selected);
    let input =
        to_raw_value(&json!({ "message": "benchmark" })).expect("benchmark input must serialize");
    c.bench_function("tool_registry_dispatch/registered_function", |benchmark| {
        benchmark.to_async(&runtime).iter(|| async {
            let output = registered
                .execute_tool(
                    "benchmark_echo",
                    ToolInput::Function(input.clone()),
                    context,
                )
                .await;
            assert!(output.success);
            std::hint::black_box(output);
        });
    });

    let code_mode = ToolRuntime::new(".", None, None);
    let warmup = runtime.block_on(code_mode.execute_code(
        r#"text("warmup")"#,
        ToolContext::new(
            "benchmark-model",
            "benchmark-session",
            "benchmark-code-mode-warmup",
            &[],
            DEFAULT_TOOL_OUTPUT_TOKENS,
        ),
    ));
    assert!(warmup.success);
    c.bench_function("code_mode_exec/warm_text", |benchmark| {
        benchmark.to_async(&runtime).iter(|| async {
            let execution = code_mode
                .execute_code(
                    r#"text("benchmark")"#,
                    ToolContext::new(
                        "benchmark-model",
                        "benchmark-session",
                        "benchmark-code-mode",
                        &[],
                        DEFAULT_TOOL_OUTPUT_TOKENS,
                    ),
                )
                .await;
            assert!(execution.success);
            std::hint::black_box(execution);
        });
    });
}

criterion_group!(benches, benchmark_process_output);
criterion_main!(benches);
