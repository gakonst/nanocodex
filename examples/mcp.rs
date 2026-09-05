use eyre::{Result, WrapErr};
use nanocodex::{
    Nanocodex, OpenAi, Thinking, Tools,
    tools::mcp::{Mcp, McpServer},
};

#[tokio::main]
async fn main() -> Result<()> {
    let api_key = std::env::var("OPENAI_API_KEY").wrap_err("OPENAI_API_KEY is required")?;
    let server = match std::env::var("NANOCODEX_MCP_URL") {
        Ok(url) => {
            let server = McpServer::http(url).description("Application-configured MCP server.");
            if std::env::var_os("NANOCODEX_MCP_BEARER_TOKEN").is_some() {
                server.bearer_token_env("NANOCODEX_MCP_BEARER_TOKEN")
            } else {
                server
            }
        }
        Err(_) => McpServer::http("https://developers.openai.com/mcp")
            .description("Search OpenAI developer documentation."),
    };
    let mcp = Mcp::builder().server("docs", server).build()?;
    let tools = Tools::builder().without_defaults().provider(mcp).build()?;
    let openai = OpenAi::new(api_key)?;
    let (agent, mut events) = Nanocodex::builder(openai)
        .instructions("Search configured MCP servers when documentation evidence is needed.")
        .thinking(Thinking::Low)
        .tools(tools)
        .build()?;

    let turn = agent
        .prompt(
            "In one Code Mode exec cell, await tools.tool_search for a relevant read-only MCP tool, then call one returned tool with tools[result.tools[0].name](arguments). Briefly summarize its result.",
        )
        .await?;
    events.write_turn_jsonl(std::io::stdout()).await?;
    eprintln!("final result: {}", turn.result().await?.final_message());
    Ok(())
}
