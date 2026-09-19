//! Mock MCP process tests: no installed CUA provider or desktop is required.
#![cfg(unix)]

use async_trait::async_trait;
use nanocodex_computer::{
    ComputerConfig, ComputerElicitationAction, ComputerElicitationHandler,
    ComputerElicitationRequest, ComputerElicitationResponse, ComputerTools,
};
use nanocodex_oai_api::tools::{Tool, ToolContext, ToolError, ToolInput};
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

fn params() -> Value {
    json!({
        "_meta": {
            "codex_approval_kind":"mcp_tool_call", "connector_id":"computer-use",
            "connector_name":"Computer Use", "persist":["session","always"],
            "progressToken":0, "riskLevel":"low", "tool_name":"get_app_state",
            "tool_params":{"app":"com.nanocodex.CuaProviderFixture"},
            "tool_params_display":[{"display_name":"App","name":"app","value":"CUA Provider Fixture"}]
        },
        "message":"Allow Computer Use to use \"CUA Provider Fixture\"?", "mode":"form",
        "requestedSchema":{"properties":{},"type":"object"}
    })
}

fn config(mode: &str, params: Value) -> ComputerConfig {
    let method = if mode == "alias" {
        "openai/elicitation/create"
    } else {
        "elicitation/create"
    };
    let request = json!({"jsonrpc":"2.0","id":"approval-0","method":method,"params":params});
    // The process echoes the actual initialize request and elicitation response
    // inside structuredContent, keeping assertions on the wire contract.
    let after_request = match mode {
        "close" => "sleep 0.1; exit 0",
        "complete" => {
            "sleep 0.1; printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"content\":[],\"structuredContent\":{\"completed\":true}}}'; IFS= read -r end; exit 0"
        }
        "cancel" => {
            "printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"method\":\"notifications/cancelled\",\"params\":{\"requestId\":\"approval-0\"}}'"
        }
        _ => ":",
    };
    let script = format!(
        r#"
IFS= read -r initialize
printf '%s\n' '{{"jsonrpc":"2.0","id":1,"result":{{"protocolVersion":"2025-06-18","capabilities":{{}}}}}}'
IFS= read -r initialized
IFS= read -r list
printf '%s\n' '{{"jsonrpc":"2.0","id":2,"result":{{"tools":[{{"name":"js","inputSchema":{{"type":"object"}}}}]}}}}'
IFS= read -r call || exit 0
printf '%s\n' '{request}'
{after_request}
IFS= read -r answer || exit 0
printf '{{"jsonrpc":"2.0","id":3,"result":{{"content":[],"structuredContent":{{"initialize":%s,"answer":%s}}}}}}\n' "$initialize" "$answer"
IFS= read -r end
"#
    );
    let mut config = ComputerConfig::mcp("/bin/sh");
    config.args = vec!["-c".into(), script.into()];
    config.elicitation_timeout = Duration::from_millis(100);
    config
}

#[derive(Debug)]
struct Host {
    response: Option<ComputerElicitationResponse>,
    seen: Mutex<Vec<ComputerElicitationRequest>>,
    dropped: Arc<AtomicBool>,
}
struct OnDrop(Arc<AtomicBool>);
impl Drop for OnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}
#[async_trait]
impl ComputerElicitationHandler for Host {
    async fn elicit(
        &self,
        request: ComputerElicitationRequest,
    ) -> Result<ComputerElicitationResponse, ToolError> {
        let _guard = OnDrop(self.dropped.clone());
        self.seen.lock().unwrap().push(request);
        match &self.response {
            Some(response) => Ok(response.clone()),
            None => std::future::pending().await,
        }
    }
}
fn host(response: Option<ComputerElicitationResponse>) -> Arc<Host> {
    Arc::new(Host {
        response,
        seen: Mutex::new(vec![]),
        dropped: Arc::new(AtomicBool::new(false)),
    })
}
async fn invoke(tools: &ComputerTools) -> Result<Value, ToolError> {
    let result = tools
        .js()
        .execute(
            ToolInput::Function(serde_json::value::to_raw_value(&json!({})).unwrap()),
            ToolContext::new(
                "fixture-model",
                "fixture-session",
                "fixture-call",
                &[],
                16000,
            ),
        )
        .await?;
    Ok(result.structured_result()["structuredContent"].clone())
}

#[tokio::test]
async fn forwards_explicit_host_actions_and_all_metadata_without_inventing_consent() {
    for action in [
        ComputerElicitationAction::Accept,
        ComputerElicitationAction::Decline,
        ComputerElicitationAction::Cancel,
    ] {
        let response = ComputerElicitationResponse {
            action,
            content: Some(json!({"confirmed":true})),
            meta: Some(json!({"host-receipt":"explicit-choice", "persist":"session"})),
        };
        let host = host(Some(response.clone()));
        let mut config = config("reply", params());
        config.elicitation_handler = Some(host.clone());
        let tools = ComputerTools::connect(config).await.unwrap();
        let wire = invoke(&tools).await.unwrap();
        assert_eq!(
            wire["initialize"]["params"]["capabilities"],
            json!({"elicitation":{"form":{}}})
        );
        assert_eq!(
            wire["answer"],
            json!({"jsonrpc":"2.0","id":"approval-0","result":response})
        );
        assert_eq!(wire["answer"]["result"]["_meta"]["persist"], "session");
        let seen = host.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].params, params());
        assert_eq!(seen[0].id, "approval-0");
        let context = seen[0].context.as_ref().unwrap();
        assert_eq!(context.session_id, "fixture-session");
        assert_eq!(context.call_id, "fixture-call");
        assert_eq!(context.model, "fixture-model");
    }
}

#[tokio::test]
async fn no_handler_advertises_no_forms_and_rejects_server_requests() {
    let tools = ComputerTools::connect(config("reply", params()))
        .await
        .unwrap();
    let wire = invoke(&tools).await.unwrap();
    assert_eq!(wire["initialize"]["params"]["capabilities"], json!({}));
    assert_eq!(wire["answer"]["error"]["code"], -32601);
}

#[tokio::test]
async fn timeout_cancels_and_drops_the_host_future() {
    let host = host(None);
    let mut config = config("reply", params());
    config.elicitation_handler = Some(host.clone());
    let tools = ComputerTools::connect(config).await.unwrap();
    let wire = tokio::time::timeout(Duration::from_secs(3), invoke(&tools))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(wire["answer"]["result"], json!({"action":"cancel"}));
    assert!(host.dropped.load(Ordering::SeqCst));
}

#[tokio::test]
async fn provider_close_cannot_wait_for_the_host_timeout() {
    let host = host(None);
    let mut config = config("close", params());
    config.elicitation_timeout = Duration::from_secs(60);
    config.elicitation_handler = Some(host.clone());
    let tools = ComputerTools::connect(config).await.unwrap();
    let result = tokio::time::timeout(Duration::from_secs(3), invoke(&tools))
        .await
        .unwrap();
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("closed its output")
    );
    tokio::task::yield_now().await;
    assert_eq!(host.seen.lock().unwrap().len(), 1);
    assert!(host.dropped.load(Ordering::SeqCst));
}

#[tokio::test]
async fn provider_cancellation_returns_cancel() {
    let host = host(None);
    let mut config = config("cancel", params());
    config.elicitation_timeout = Duration::from_secs(60);
    config.elicitation_handler = Some(host);
    let tools = ComputerTools::connect(config).await.unwrap();
    let wire = tokio::time::timeout(Duration::from_secs(3), invoke(&tools))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(wire["answer"]["result"], json!({"action":"cancel"}));
}

#[tokio::test]
async fn rejects_url_and_malformed_forms_without_calling_host() {
    for params in [
        json!({"mode":"url","message":"Open URL", "url":"https://example.com"}),
        json!({"mode":"form","message":3,"requestedSchema":{}}),
        json!({"mode":"form","message":"Form","requestedSchema":{},"_meta":[]}),
    ] {
        let host = host(None);
        let mut config = config("reply", params);
        config.elicitation_handler = Some(host.clone());
        let tools = ComputerTools::connect(config).await.unwrap();
        let wire = invoke(&tools).await.unwrap();
        assert_eq!(wire["answer"]["error"]["code"], -32602);
        assert!(host.seen.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn caller_cancellation_drops_pending_host_ui_and_requires_reset() {
    let host = host(None);
    let mut config = config("reply", params());
    config.elicitation_timeout = Duration::from_secs(60);
    config.elicitation_handler = Some(host.clone());
    let tools = ComputerTools::connect(config).await.unwrap();
    let active = tools.clone();
    let call = tokio::spawn(async move { invoke(&active).await });
    tokio::time::timeout(Duration::from_secs(15), async {
        while host.seen.lock().unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    call.abort();
    let _ = call.await;
    tokio::time::timeout(Duration::from_secs(3), async {
        while !host.dropped.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        invoke(&tools)
            .await
            .unwrap_err()
            .to_string()
            .contains("js_reset")
    );
}

#[tokio::test]
async fn invalid_host_content_or_metadata_cannot_approve() {
    for (content, meta) in [(Some(json!([])), None), (None, Some(json!([])))] {
        let host = host(Some(ComputerElicitationResponse {
            action: ComputerElicitationAction::Accept,
            content,
            meta,
        }));
        let mut config = config("reply", params());
        config.elicitation_handler = Some(host);
        let tools = ComputerTools::connect(config).await.unwrap();
        assert_eq!(
            invoke(&tools).await.unwrap()["answer"]["error"]["code"],
            -32602
        );
    }
}

#[derive(Debug)]
struct FailingHost;
#[async_trait]
impl ComputerElicitationHandler for FailingHost {
    async fn elicit(
        &self,
        _: ComputerElicitationRequest,
    ) -> Result<ComputerElicitationResponse, ToolError> {
        Err("private host detail".into())
    }
}
#[tokio::test]
async fn host_failure_is_redacted_and_never_approves() {
    let mut config = config("reply", params());
    config.elicitation_handler = Some(Arc::new(FailingHost));
    let tools = ComputerTools::connect(config).await.unwrap();
    let wire = invoke(&tools).await.unwrap();
    assert_eq!(wire["answer"]["error"]["code"], -32603);
    assert!(!wire.to_string().contains("private host detail"));
}

#[tokio::test]
async fn omitted_mode_is_form_and_does_not_inherit_persistence() {
    let mut request = params();
    request.as_object_mut().unwrap().remove("mode");
    let mut config = config("reply", request);
    config.elicitation_handler = Some(host(Some(ComputerElicitationResponse {
        action: ComputerElicitationAction::Accept,
        content: None,
        meta: None,
    })));
    let tools = ComputerTools::connect(config).await.unwrap();
    assert_eq!(
        invoke(&tools).await.unwrap()["answer"]["result"],
        json!({"action":"accept"})
    );
}

#[tokio::test]
async fn call_completion_drops_pending_host_ui() {
    let host = host(None);
    let mut config = config("complete", params());
    config.elicitation_timeout = Duration::from_secs(60);
    config.elicitation_handler = Some(host.clone());
    let tools = ComputerTools::connect(config).await.unwrap();
    assert_eq!(invoke(&tools).await.unwrap(), json!({"completed":true}));
    tokio::time::timeout(Duration::from_secs(3), async {
        while !host.dropped.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn codex_alias_uses_the_same_form_handler_and_validation() {
    for mode in ["form", "url"] {
        let mut params = params();
        params["mode"] = json!(mode);
        let response = ComputerElicitationResponse {
            action: ComputerElicitationAction::Accept,
            content: Some(json!({})),
            meta: Some(json!({"receipt":"explicit-host-choice"})),
        };
        let host = host(Some(response.clone()));
        let mut config = config("alias", params.clone());
        config.elicitation_handler = Some(host.clone());
        let tools = ComputerTools::connect(config).await.unwrap();
        let wire = invoke(&tools).await.unwrap();
        if mode == "form" {
            assert_eq!(host.seen.lock().unwrap()[0].params, params);
            assert_eq!(
                wire["answer"]["result"],
                serde_json::to_value(response).unwrap()
            );
        } else {
            assert!(host.seen.lock().unwrap().is_empty());
            assert_eq!(wire["answer"]["error"]["code"], -32602);
        }
    }
}
