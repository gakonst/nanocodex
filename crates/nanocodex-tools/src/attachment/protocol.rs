use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::AttachmentMachine;

pub(crate) const MAX_FRAME_BYTES: usize = 256 * 1024;
pub(crate) const MAX_OUTPUT_BYTES: u64 = 128 * 1024;
pub(crate) const MAX_IN_FLIGHT: usize = 64;
pub(crate) const MAX_RECEIPTS: usize = 512;
#[cfg(not(test))]
pub(crate) const HEARTBEAT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(20);
#[cfg(test)]
pub(crate) const HEARTBEAT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(200);

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ExecutorFrame<'a> {
    Catalog {
        tools: &'a Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        machines: Option<&'a [AttachmentMachine]>,
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
        if text.len() > MAX_FRAME_BYTES {
            return Err("frame exceeds 256 KiB");
        }
        let frame: Self = serde_json::from_str(text).map_err(|_| "invalid attachment frame")?;
        frame.validate()?;
        Ok(frame)
    }

    fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::Ready {} | Self::Draining {} => Ok(()),
            Self::Call {
                session_id,
                call_id,
                model,
                name,
                input,
                output_token_budget,
                output_byte_budget,
                deadline_at,
            } => {
                if !valid_identifier(session_id)
                    || !valid_identifier(call_id)
                    || !valid_identifier(model)
                    || !valid_tool_name(name)
                    || !(input.is_object() || input.is_string())
                    || serde_json::to_vec(input).map_or(true, |value| value.len() > 128 * 1024)
                    || !(1..=1_000_000).contains(output_token_budget)
                    || !(1..=MAX_OUTPUT_BYTES).contains(output_byte_budget)
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
        assert!(RemoteFrame::parse(&frame.replace("\"model\":\"gpt-5.6-sol\",", "")).is_err());
    }
}
