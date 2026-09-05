use std::{path::Path, sync::Arc};

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use nanocodex_tools::{
    Tool, ToolContext, ToolInput,
    contract::DEFAULT_TOOL_OUTPUT_TOKENS,
    mcp::{Mcp, McpServer},
    runtime::DynamicToolProvider,
};
use serde_json::{json, value::to_raw_value};
use tokio::runtime::Runtime;

fn search_tool(provider: &Mcp) -> Arc<dyn Tool> {
    provider
        .direct_tools()
        .into_iter()
        .find(|tool| tool.definition().name() == "tool_search")
        .expect("MCP must expose tool_search under direct exposure")
}

fn warm_provider(
    runtime: &Runtime,
    tool_count: usize,
) -> (Mcp, Arc<dyn Tool>, Box<serde_json::value::RawValue>) {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mcp-stdio-server.mjs");
    let provider = Mcp::builder()
        .server(
            "fixture",
            McpServer::stdio("node")
                .arg(fixture.to_string_lossy())
                .env("NANOCODEX_MCP_FIXTURE_TOOL_COUNT", tool_count.to_string()),
        )
        .build()
        .expect("benchmark MCP configuration must be valid");
    {
        let _runtime = runtime.enter();
        provider.start();
    }
    let search = search_tool(&provider);
    let input = to_raw_value(&json!({ "query": "deterministic echo message", "limit": 8 }))
        .expect("benchmark input must serialize");
    let context = ToolContext::new(
        "benchmark-model",
        "benchmark-session",
        "benchmark-search",
        &[],
        DEFAULT_TOOL_OUTPUT_TOKENS,
    );
    let warmup = runtime
        .block_on(search.execute(ToolInput::Function(input.clone()), context))
        .expect("warm MCP search must execute");
    assert!(warmup.success);
    (provider, search, input)
}

fn benchmark_search(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("benchmark Tokio runtime must initialize");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mcp-stdio-server.mjs");
    let mut discovery = c.benchmark_group("mcp_discovery/cold");
    discovery.sample_size(10);
    discovery.bench_function("1000_tools", |benchmark| {
        benchmark.to_async(&runtime).iter(|| async {
            let provider = Mcp::builder()
                .server(
                    "fixture",
                    McpServer::stdio("node")
                        .arg(fixture.to_string_lossy())
                        .env("NANOCODEX_MCP_FIXTURE_TOOL_COUNT", "1000"),
                )
                .build()
                .expect("benchmark MCP configuration must be valid");
            provider.start();
            let search = search_tool(&provider);
            let input = to_raw_value(&json!({ "query": "deterministic echo message", "limit": 8 }))
                .expect("benchmark input must serialize");
            let result = search
                .execute(
                    ToolInput::Function(input),
                    ToolContext::new(
                        "benchmark-model",
                        "benchmark-session",
                        "benchmark-discovery",
                        &[],
                        DEFAULT_TOOL_OUTPUT_TOKENS,
                    ),
                )
                .await
                .expect("cold MCP discovery and search must execute");
            assert!(result.success);
            std::hint::black_box((provider, result));
        });
    });
    discovery.finish();

    let mut group = c.benchmark_group("mcp_tool_search/warm");
    for tool_count in [1, 1_000] {
        let (_provider, search, input) = warm_provider(&runtime, tool_count);
        let context = ToolContext::new(
            "benchmark-model",
            "benchmark-session",
            "benchmark-search",
            &[],
            DEFAULT_TOOL_OUTPUT_TOKENS,
        );
        group.bench_with_input(
            BenchmarkId::from_parameter(tool_count),
            &tool_count,
            |benchmark, _| {
                benchmark.to_async(&runtime).iter(|| async {
                    let result = search
                        .execute(ToolInput::Function(input.clone()), context)
                        .await
                        .expect("MCP search must execute");
                    std::hint::black_box(result);
                });
            },
        );
    }
    group.finish();

    let (provider, _search, _) = warm_provider(&runtime, 1);
    let context = ToolContext::new(
        "benchmark-model",
        "benchmark-session",
        "benchmark-dispatch",
        &[],
        DEFAULT_TOOL_OUTPUT_TOKENS,
    );
    let input = json!({ "message": "benchmark" });
    c.bench_function("mcp_tool_dispatch/warm_stdio", |benchmark| {
        benchmark.to_async(&runtime).iter(|| async {
            let result = provider
                .execute("mcp__fixture__echo", input.clone(), context)
                .await
                .expect("activated MCP tool must execute");
            assert!(result.success);
            std::hint::black_box(result);
        });
    });
}

criterion_group!(benches, benchmark_search);
criterion_main!(benches);
