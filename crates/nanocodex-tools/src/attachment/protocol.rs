use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::AttachmentMachine;

#[cfg(not(test))]
pub(crate) const HEARTBEAT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(20);
#[cfg(test)]
pub(crate) const HEARTBEAT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(200);

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ExecutorFrame<'a> {
    Catalog {
        capabilities: &'a [&'a str],
        tools: &'a Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        machines: Option<&'a [AttachmentMachine]>,
        #[serde(skip_serializing_if = "Option::is_none")]
        attachment_id: Option<&'a str>,
    },
    Result {
        call_id: &'a str,
        outcome: &'a Value,
    },
    Ping {
        nonce: &'a str,
    },
    Drain {},
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum RemoteFrame {
    Ready {},
    Call {
        session_id: String,
        #[serde(default, deserialize_with = "deserialize_turn_id")]
        turn_id: Option<String>,
        call_id: String,
        model: String,
        name: String,
        input: Value,
        output_token_budget: u64,
        output_byte_budget: u64,
        deadline_at: u64,
    },
    Cancel {
        call_id: String,
    },
    Ack {
        call_id: String,
    },
    Pong {
        nonce: String,
    },
    Draining {},
}

impl RemoteFrame {
    pub(crate) const fn kind(&self) -> &'static str {
        match self {
            Self::Ready {} => "ready",
            Self::Call { .. } => "call",
            Self::Cancel { .. } => "cancel",
            Self::Ack { .. } => "ack",
            Self::Pong { .. } => "pong",
            Self::Draining {} => "draining",
        }
    }

    pub(crate) fn parse(text: &str) -> Result<Self, &'static str> {
        let frame: Self = serde_json::from_str(text).map_err(|_| "invalid attachment frame")?;
        frame.validate()?;
        Ok(frame)
    }

    fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::Ready {} | Self::Draining {} => Ok(()),
            Self::Call {
                session_id,
                turn_id,
                call_id,
                model,
                name,
                input,
                output_token_budget,
                output_byte_budget,
                deadline_at,
            } => {
                if !valid_identifier(session_id)
                    || turn_id
                        .as_ref()
                        .is_some_and(|turn| turn.is_empty() || turn.len() > 256)
                    || !valid_identifier(call_id)
                    || !valid_identifier(model)
                    || !valid_tool_name(name)
                    || !(input.is_object() || input.is_string())
                    || !positive(*output_token_budget)
                    || !positive(*output_byte_budget)
                    || !positive(*deadline_at)
                {
                    return Err("invalid call");
                }
                Ok(())
            }
            Self::Cancel { call_id } | Self::Ack { call_id } => {
                if valid_identifier(call_id) {
                    Ok(())
                } else {
                    Err("invalid call identity")
                }
            }
            Self::Pong { nonce } => {
                if nonce.is_empty() || nonce.len() > 128 {
                    Err("invalid pong")
                } else {
                    Ok(())
                }
            }
        }
    }
}

fn deserialize_turn_id<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    String::deserialize(deserializer).map(Some)
}

const fn positive(value: u64) -> bool {
    value > 0 && value <= 9_007_199_254_740_991
}

fn valid_identifier(value: &str) -> bool {
    value.len() <= 128
        && value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

fn valid_tool_name(value: &str) -> bool {
    valid_identifier(value) && !matches!(value, "exec" | "tool_search" | "wait")
}

#[cfg(test)]
mod tests {
    use super::{ExecutorFrame, RemoteFrame};

    #[test]
    fn frames_use_only_the_socket_owned_protocol() {
        assert_eq!(
            serde_json::to_value(ExecutorFrame::Drain {}).unwrap(),
            serde_json::json!({"type":"drain"})
        );
        assert!(matches!(
            RemoteFrame::parse(r#"{"type":"ready"}"#),
            Ok(RemoteFrame::Ready {})
        ));
        assert!(RemoteFrame::parse(r#"{"type":"ready","protocol_version":1}"#).is_err());
    }

    #[test]
    fn parses_and_bounds_calls() {
        let frame = r#"{"type":"call","session_id":"session:1","call_id":"call:1","model":"gpt-5.6-sol","name":"lookup","input":{},"output_token_budget":1000,"output_byte_budget":131072,"deadline_at":1}"#;
        assert!(matches!(
            RemoteFrame::parse(frame),
            Ok(RemoteFrame::Call { model, .. }) if model == "gpt-5.6-sol"
        ));
        let mut with_turn: serde_json::Value = serde_json::from_str(frame).unwrap();
        with_turn["turn_id"] = serde_json::json!("session:1:7");
        assert!(
            matches!(RemoteFrame::parse(&with_turn.to_string()), Ok(RemoteFrame::Call { turn_id: Some(turn), .. }) if turn == "session:1:7")
        );
        for invalid in [
            serde_json::json!(""),
            serde_json::json!("é".repeat(129)),
            serde_json::json!(null),
            serde_json::json!(7),
        ] {
            with_turn["turn_id"] = invalid;
            assert!(RemoteFrame::parse(&with_turn.to_string()).is_err());
        }
        assert!(RemoteFrame::parse(&frame.replace("\"model\":\"gpt-5.6-sol\",", "")).is_err());
    }
}
