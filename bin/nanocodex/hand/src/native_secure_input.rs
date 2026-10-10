//! Ciphertext-only transport to the separately enrolled root helper.

// Boundary scenarios defined before implementation: reject plaintext and unknown
// fields before socket access; bound framing rejects oversized/trailing output;
// peer identity must be root; failures never reflect helper-controlled errors.
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn plaintext_and_extra_fields_are_never_admitted() {
        for input in [
            json!({"operation":"submit","request_id":"x","value":"synthetic"}),
            json!({"operation":"cancel","request_id":"x","value":"synthetic"}),
            json!({"operation":"prepare","executable":"/usr/bin/id","arguments":[],"cwd":"/","value":"synthetic"}),
        ] {
            assert!(serde_json::from_value::<Request>(input).is_err());
        }
    }
    #[tokio::test]
    async fn bounded_frame_accepts_one_object_and_rejects_echo() {
        use tokio::io::AsyncWriteExt;
        for (wire, valid) in [
            (b"{\"status\":\"rejected\"}\n".to_vec(), true),
            (b"{\"status\":\"rejected\"}\nprivate-echo".to_vec(), false),
            (vec![b'x'; 32769], false),
        ] {
            let (mut sender, receiver) = tokio::io::duplex(65536);
            sender.write_all(&wire).await.unwrap();
            drop(sender);
            assert_eq!(read_frame(receiver).await.is_ok(), valid);
        }
    }
    #[test]
    fn only_fixed_receipts_or_signed_ticket_fields_survive() {
        assert!(
            serde_json::from_value::<Response>(
                json!({"status":"rejected","error":"synthetic-echo"})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<Response>(
                json!({"request_id":"x","status":"completed","exit_code":0})
            )
            .is_ok()
        );
    }
}

use nanocodex_oai_tools::{Tool, ToolContext, ToolDefinition, ToolInput, ToolOutput, ToolResult};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{io, path::Path, time::Duration};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
#[cfg(target_os = "macos")]
const SOCKET: &str = "/var/run/nanocodex-secure-input.sock";
#[cfg(target_os = "linux")]
const SOCKET: &str = "/run/nanocodex-secure-input.sock";
const MAX_FRAME: u64 = 32768;
fn unavailable() -> io::Error {
    io::Error::other("Native secure input unavailable")
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Prepare {
        executable: String,
        arguments: Vec<String>,
        cwd: String,
    },
    Submit {
        request_id: String,
        ephemeral_public_key: String,
        ciphertext: String,
        signature: String,
    },
    Cancel {
        request_id: String,
    },
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Command {
    executable: String,
    arguments: Vec<String>,
    cwd: String,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Ticket {
    request_id: String,
    command_digest: String,
    public_key: String,
    expires_at: u64,
    uid: u32,
    command: Command,
    helper_signature: String,
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Status {
    Completed,
    OutcomeUnknown,
    Cancelled,
    Unavailable,
    Rejected,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    request_id: Option<String>,
    status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
}
#[derive(Deserialize, Serialize)]
#[serde(untagged)]
enum Response {
    Ticket(Ticket),
    Receipt(Receipt),
}

async fn read_frame(reader: impl AsyncRead + Unpin) -> io::Result<Response> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_FRAME + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| unavailable())?;
    if bytes.len() as u64 > MAX_FRAME
        || bytes.last() != Some(&b'\n')
        || bytes[..bytes.len().saturating_sub(1)].contains(&b'\n')
    {
        return Err(unavailable());
    }
    serde_json::from_slice(&bytes).map_err(|_| unavailable())
}

pub(super) struct NativeSecureInput;
impl NativeSecureInput {
    pub(super) fn installed() -> bool {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        std::fs::symlink_metadata(Path::new(SOCKET))
            .is_ok_and(|m| m.file_type().is_socket() && m.uid() == 0 && m.mode() & 0o7777 == 0o666)
    }
    async fn exchange(input: Request) -> io::Result<Response> {
        if !Self::installed() {
            return Err(unavailable());
        }
        let mut bytes = serde_json::to_vec(&input).map_err(|_| unavailable())?;
        bytes.push(b'\n');
        if bytes.len() as u64 > MAX_FRAME {
            return Err(unavailable());
        }
        let operation = async {
            let mut stream = tokio::net::UnixStream::connect(SOCKET)
                .await
                .map_err(|_| unavailable())?;
            if stream.peer_cred().map_err(|_| unavailable())?.uid() != 0 {
                return Err(unavailable());
            }
            stream.write_all(&bytes).await.map_err(|_| unavailable())?;
            stream.shutdown().await.map_err(|_| unavailable())?;
            read_frame(stream).await
        };
        tokio::time::timeout(Duration::from_secs(135), operation)
            .await
            .map_err(|_| unavailable())?
    }
}
#[async_trait::async_trait]
impl Tool for NativeSecureInput {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "native_secure_input",
            "Transport public tickets and encrypted approvals to this computer's independently enrolled protected helper. Password plaintext is never accepted. Use request_native_secure_input for mobile authorization.",
            json!({
                "type":"object", "properties": {
                    "operation":{"type":"string","enum":["prepare","submit","cancel"]},
                    "executable":{"type":"string"}, "arguments":{"type":"array","items":{"type":"string"}}, "cwd":{"type":"string"},
                    "request_id":{"type":"string"}, "ephemeral_public_key":{"type":"string"}, "ciphertext":{"type":"string"}, "signature":{"type":"string"}
                }, "required":["operation"], "additionalProperties":false
            }),
        )
    }
    async fn execute(&self, input: ToolInput, _context: ToolContext<'_>) -> ToolResult {
        // Do not reflect deserialization errors, which may contain rejected input.
        let input = input.decode_json::<Request>().map_err(|_| unavailable())?;
        let result = Self::exchange(input).await?;
        let value = serde_json::to_value(result).map_err(|_| unavailable())?;
        Ok(ToolOutput::text(value.to_string()).with_structured_result(value))
    }
}
