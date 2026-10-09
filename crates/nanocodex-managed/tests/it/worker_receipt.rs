//! Invoked by the production workerd/curl journey, never a simulated API server.
use nanocodex_agent::{Nanocodex, PromptRequest};
use nanocodex_managed::{AgentSettings, Managed, ManagedApiKey, ManagedClient};
use serde::Deserialize;

#[derive(Deserialize)]
struct Journey {
    origin: String,
    api_key: String,
    input: String,
    key: String,
    settings: AgentSettings,
    reject: bool,
}

#[tokio::test]
#[ignore = "requires the production worker fixture; run node js/managed/test/large-run-receipt-journey.test.mjs"]
async fn production_worker_receipt() {
    let path = std::env::var("NANOCODEX_RECEIPT_JOURNEY").expect("journey configuration path");
    let fixture: Journey = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let client = ManagedClient::new(
        fixture.origin,
        ManagedApiKey::parse(fixture.api_key).unwrap(),
    )
    .unwrap();
    let result = Nanocodex::builder(Managed::create(client).with_settings(fixture.settings))
        .build_with_prompt(fixture.input, fixture.key)
        .await;
    if fixture.reject {
        let error = match result {
            Ok(_) => panic!("oversized receipt unexpectedly accepted"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("receipt exceeds size limit"),
            "{error}"
        );
        println!("JOURNEY bounded receipt rejected: {error}");
        return;
    }
    let (agent, _, turn) = result.expect("valid fragmented initial receipt");
    assert_eq!(
        turn.result().await.unwrap().final_message(),
        "BENCHMARK_ASSISTANT_TEXT"
    );
    let followup = agent
        .prompt(PromptRequest::new(
            "BENCH_SAMPLE_receipt_followup: reply with the fixture text",
        ))
        .await
        .unwrap();
    assert_eq!(
        followup.result().await.unwrap().final_message(),
        "BENCHMARK_ASSISTANT_TEXT"
    );
    println!(
        "JOURNEY receipt and subsequent work completed: {}",
        agent.agent_id()
    );
    agent.disconnect().await.unwrap();
}
