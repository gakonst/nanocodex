use super::*;
use crate::MessageRole;
use serde_json::{Value, json};

fn item(value: Value) -> ResponseItem {
    serde_json::from_value(value).unwrap()
}

fn message(role: MessageRole, text: &str) -> ResponseItem {
    ResponseItem::message(role, [ContentItem::InputText { text: text.into() }])
}

fn summary() -> ResponseItem {
    item(json!({"type":"compaction", "encrypted_content":"summary"}))
}

#[test]
fn canonical_context_precedes_latest_input_even_with_later_developer_messages() {
    let user = message(MessageRole::User, "latest task");
    let mut developer = message(MessageRole::Developer, "client state after user");
    developer.set_id(Some("client-dev".into()));
    let provenance = BTreeSet::from(["client-dev".to_owned()]);
    let context = message(MessageRole::Developer, "fresh canonical context");
    let history = vec![user.clone(), developer.clone()];
    assert_eq!(
        serde_json::to_value(install_history_with_provenance(
            &history,
            std::slice::from_ref(&context),
            summary(),
            &provenance
        ))
        .unwrap(),
        serde_json::to_value(vec![context.clone(), user, developer.clone(), summary()]).unwrap(),
    );
    assert_eq!(
        serde_json::to_value(install_history_with_provenance(
            std::slice::from_ref(&developer),
            std::slice::from_ref(&context),
            summary(),
            &provenance
        ))
        .unwrap(),
        serde_json::to_value(vec![developer, context, summary()]).unwrap(),
    );
}

#[test]
fn retained_agent_messages_follow_upstream_progress_completion_and_size_rules() {
    let agent = |author: &str, text: String| {
        item(json!({
            "type":"agent_message", "author":author, "recipient":"root",
            "content":[{"type":"input_text", "text":text}]
        }))
    };
    let task = agent("parent", "task instructions".into());
    let peer = agent("peer", "Message Type: MESSAGE\ncoordination".into());
    let history = vec![
        task.clone(),
        agent("root/child", "Message Type: MESSAGE\nprogress".into()),
        agent("peer", "Message Type: FINAL_ANSWER\ncompleted".into()),
        agent("parent", "x".repeat(40_001)),
        peer.clone(),
    ];
    let context = message(MessageRole::Developer, "context");
    assert_eq!(
        serde_json::to_value(install_history(
            &history,
            std::slice::from_ref(&context),
            summary()
        ))
        .unwrap(),
        serde_json::to_value(vec![task, context, peer, summary()]).unwrap(),
    );
}

#[test]
fn image_boundary_preserves_latest_content_and_removes_labels_atomically() {
    let input = item(json!({"type":"message", "role":"user", "content":[
        {"type":"input_text", "text":"old text"},
        {"type":"input_text", "text":"<image name=[Image #1] path=\"/old.png\">"},
        {"type":"input_image", "image_url":"data:image/png;base64,YQ=="},
        {"type":"input_text", "text":"</image>"},
        {"type":"input_text", "text":"latest task"}
    ]}));
    let result = truncate_retained_messages(vec![input], 10);
    assert_eq!(
        serde_json::to_value(result).unwrap(),
        serde_json::to_value(vec![message(MessageRole::User, "latest task")]).unwrap(),
    );
}

#[test]
fn oversized_latest_image_does_not_backfill_older_messages() {
    let image = item(json!({"type":"message", "role":"user", "content":[
        {"type":"input_image", "image_url":"data:image/png;base64,YQ=="}
    ]}));
    assert!(
        truncate_retained_messages(vec![message(MessageRole::User, "older"), image], 100)
            .is_empty()
    );
}

#[test]
fn many_retained_images_respect_the_compaction_budget() {
    let image = || {
        item(json!({"type":"message", "role":"user", "content":[
            {"type":"input_image", "image_url":"data:image/png;base64,YQ=="}
        ]}))
    };
    let retained = truncate_retained_messages(
        (0..50).map(|_| image()).collect(),
        RETAINED_MESSAGE_TOKEN_BUDGET,
    );
    assert_eq!(
        retained.len(),
        RETAINED_MESSAGE_TOKEN_BUDGET / approx_tokens(RESIZED_IMAGE_BYTES_ESTIMATE)
    );
    assert!(
        retained
            .iter()
            .map(images::message_content_token_count)
            .sum::<usize>()
            <= RETAINED_MESSAGE_TOKEN_BUDGET
    );
}

#[test]
fn retained_client_developer_messages_charge_model_visible_content() {
    let mut developer = message(MessageRole::Developer, &"x".repeat(400));
    developer.set_id(Some("client-dev".into()));
    let provenance = BTreeSet::from(["client-dev".to_owned()]);
    let retained = truncate_retained_messages_with_provenance(vec![developer], 50, &provenance);
    assert_eq!(retained.len(), 1);
    assert!(estimate_item_tokens(&retained[0]) <= 50);
}

#[test]
fn tool_output_trimming_consumes_its_attached_resize_notice() {
    let output = item(json!({"type":"custom_tool_call_output", "call_id":"call",
        "output":"x".repeat(4_000)}));
    let notice = message(
        MessageRole::Developer,
        "<image_resize_notice>Image in preceding tool output was resized.</image_resize_notice>",
    );
    let user = message(MessageRole::User, "task");
    let mut history = ResponseHistory::new(vec![user.clone(), output, notice]);
    assert_eq!(
        trim_tool_outputs_to_fit_context_window(&mut history, &[], 100),
        1
    );
    let rewritten = history.iter().collect::<Vec<_>>();
    assert_eq!(rewritten.len(), 2);
    assert_eq!(
        serde_json::to_value(rewritten[0]).unwrap(),
        serde_json::to_value(user).unwrap()
    );
    assert!(
        serde_json::to_string(rewritten[1])
            .unwrap()
            .contains(CONTEXT_WINDOW_TRUNCATED_OUTPUT_MESSAGE)
    );
}

#[test]
fn resize_notice_survives_only_with_its_retained_source() {
    let user = message(MessageRole::User, "task");
    let user_notice = message(
        MessageRole::Developer,
        "<image_resize_notice>user image resized</image_resize_notice>",
    );
    let output = item(json!({"type":"custom_tool_call_output", "call_id":"call", "output":"done"}));
    let output_notice = message(
        MessageRole::Developer,
        "<image_resize_notice>tool image resized</image_resize_notice>",
    );
    assert_eq!(
        serde_json::to_value(install_history(
            &[user.clone(), user_notice.clone(), output, output_notice],
            &[],
            summary()
        ))
        .unwrap(),
        serde_json::to_value(vec![user, user_notice, summary()]).unwrap(),
    );
}

#[test]
fn trimming_uses_ninety_five_percent_but_auto_trigger_uses_ninety_percent() {
    let raw_window = 1_000;
    assert_eq!(
        auto_compact_token_limit("gpt-6-astra", raw_window),
        Some(900)
    );
    for (tokens, rewritten) in [(950, 0), (951, 1)] {
        let mut history = ResponseHistory::new(vec![ResponseItem::custom_tool_output(
            "call".to_owned(),
            None,
            FunctionOutputBody::Text("x".repeat(tokens * 4 - 4).into_boxed_str()),
        )]);
        assert_eq!(
            estimate_item_tokens(history.iter().next().unwrap()),
            tokens as u64
        );
        assert_eq!(
            trim_tool_outputs_to_fit_context_window(&mut history, &[], raw_window),
            rewritten
        );
    }
}

#[test]
fn developer_retention_uses_provenance_even_when_client_text_looks_like_harness_context() {
    let harness = message(MessageRole::Developer, "arbitrary generated context");
    let mut client = message(
        MessageRole::Developer,
        "<image_resize_notice>client instructions</image_resize_notice>",
    );
    client.set_id(Some("explicit-client".into()));
    let ids = BTreeSet::from(["explicit-client".to_owned()]);
    let history = vec![harness, client.clone()];
    assert_eq!(
        serde_json::to_value(install_history_with_provenance(
            &history,
            &[],
            summary(),
            &ids
        ))
        .unwrap(),
        serde_json::to_value(vec![client, summary()]).unwrap()
    );
    // An old snapshot without provenance is not migrated by guessing from text.
    assert_eq!(install_history(&history, &[], summary()).len(), 1);
}
