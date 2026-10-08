//! Subscription wire compatibility ported from oh-my-pi v18.4.4.
//! Sources: packages/ai/src/providers/{anthropic,anthropic-identity,claude-code-fingerprint}.ts.
//! Copyright (c) 2025 Mario Zechner; 2025-2026 Can Bölük; 2026 Stencil Labs, Inc.
//! MIT license: ../THIRD-PARTY-LICENSES (included in the crate package).
use crate::{ClaudeError, ContentBlock, MessagesRequest, Role};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub const CLAUDE_CODE_VERSION: &str = "2.1.280";
pub const CLAUDE_CODE_SDK_VERSION: &str = "0.112.1";
pub const IDENTITY: &str = "You are Claude Code, Anthropic's official CLI for Claude.";
const BILLING: &str = "x-anthropic-billing-header:";
const PLACEHOLDER: &[u8] = b"cch=00000";
const MARKER: &[u8] = b"\"system\":[{\"type\":\"text\",\"text\":\"x-anthropic-billing-header:";

/// Public host identity, never an OAuth credential. Supply the same installation
/// identity on reopen; session affinity is bound by the native backend.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubscriptionIdentity {
    pub install_id: Option<String>,
    pub account_uuid: Option<String>,
    pub user_id: Option<String>,
    pub platform: Option<String>,
    pub arch: Option<String>,
    pub version: Option<String>,
}
impl SubscriptionIdentity {
    pub fn validate(&self) -> Result<(), ClaudeError> {
        for value in [
            &self.install_id,
            &self.account_uuid,
            &self.user_id,
            &self.platform,
            &self.arch,
            &self.version,
        ]
        .into_iter()
        .flatten()
        {
            if value.is_empty() || value.len() > 8192 || value.chars().any(char::is_control) {
                return Err(ClaudeError::Protocol(
                    "invalid subscription wire identity".into(),
                ));
            }
        }
        Ok(())
    }
    pub fn version(&self) -> &str {
        self.version.as_deref().unwrap_or(CLAUDE_CODE_VERSION)
    }
    pub fn os(&self) -> String {
        let platform = self
            .platform
            .as_deref()
            .unwrap_or(if cfg!(target_os = "macos") {
                "darwin"
            } else if cfg!(target_os = "windows") {
                "win32"
            } else {
                "linux"
            });
        match platform.to_lowercase().as_str() {
            "darwin" => "MacOS".into(),
            "windows" | "win32" => "Windows".into(),
            "linux" => "Linux".into(),
            "freebsd" => "FreeBSD".into(),
            other => format!("Other::{other}"),
        }
    }
    pub fn arch(&self) -> String {
        let arch = self
            .arch
            .as_deref()
            .unwrap_or(if cfg!(target_arch = "aarch64") {
                "arm64"
            } else if cfg!(target_arch = "x86") {
                "ia32"
            } else {
                "x64"
            });
        match arch.to_lowercase().as_str() {
            "amd64" | "x64" => "x64".into(),
            "arm64" | "aarch64" => "arm64".into(),
            "386" | "x86" | "ia32" => "x86".into(),
            other => format!("other::{other}"),
        }
    }
    pub fn metadata(&self, session: &str) -> Result<Value, ClaudeError> {
        if let Some(user) = &self.user_id {
            let json_id = user.starts_with('{')
                && serde_json::from_str::<Value>(user).ok().is_some_and(|v| {
                    v.get("session_id")
                        .and_then(Value::as_str)
                        .is_some_and(|id| !id.is_empty())
                });
            let legacy = regex::Regex::new(r"^user_[0-9a-fA-F]{64}_account_[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}_session_[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$").expect("pinned OMP pattern");
            if json_id || legacy.is_match(user) {
                return Ok(json!({"user_id":user}));
            }
        }
        let install = self.install_id.as_deref().unwrap_or(session);
        let input = self.account_uuid.as_ref().map_or_else(
            || format!("omp-claude-device-id-v1:{install}"),
            |account| format!("omp-claude-device-id-v2\0{install}\0{account}"),
        );
        #[derive(Serialize)]
        struct User<'a> {
            device_id: String,
            session_id: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            account_uuid: Option<&'a str>,
        }
        let user = User {
            device_id: hex_digest(input.as_bytes()),
            session_id: session,
            account_uuid: self.account_uuid.as_deref(),
        };
        Ok(json!({"user_id":serde_json::to_string(&user)?}))
    }
}
fn hex_digest(input: &[u8]) -> String {
    Sha256::digest(input)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
pub fn prefix(name: &str) -> String {
    if matches!(
        name.to_lowercase().as_str(),
        "web_search" | "code_execution" | "text_editor" | "computer"
    ) {
        name.into()
    } else {
        format!("_{name}")
    }
}
pub fn strip(name: &mut String) {
    if name.starts_with('_') {
        name.remove(0);
    }
}
fn fingerprint(request: &MessagesRequest, version: &str) -> String {
    let first = request
        .messages
        .iter()
        .find(|m| m.role == Role::User)
        .and_then(|m| {
            m.content.iter().find_map(|b| match b {
                ContentBlock::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
        })
        .unwrap_or("");
    let units: Vec<u16> = first.encode_utf16().collect();
    let selected = [4, 7, 20].map(|i| units.get(i).copied().unwrap_or(u16::from(b'0')));
    let seed = format!(
        "59cf53e54c78{}{version}",
        String::from_utf16_lossy(&selected)
    );
    hex_digest(seed.as_bytes())[..3].into()
}
pub fn prepare(request: &mut MessagesRequest, identity: &SubscriptionIdentity) {
    let mut existing = match request.system.take() {
        Some(Value::String(s)) => vec![json!({"type":"text","text":s.trim()})],
        Some(Value::Array(v)) => v,
        Some(v) => vec![v],
        None => vec![],
    };
    let generated = existing
        .first()
        .and_then(|v| v.get("text"))
        .and_then(Value::as_str)
        .is_some_and(|s| s.starts_with(BILLING))
        && existing
            .get(1)
            .and_then(|v| v.get("text"))
            .and_then(Value::as_str)
            == Some(IDENTITY);
    if generated {
        existing.drain(..2);
    } else if existing.iter().any(|v| {
        v.get("text")
            .and_then(Value::as_str)
            .is_some_and(|s| s.starts_with(BILLING))
    }) {
        request.system = Some(Value::Array(existing));
        return;
    }
    // Migrate the former explicit Nanocodex protocol prefix without duplication.
    if existing
        .first()
        .and_then(|v| v.get("text"))
        .and_then(Value::as_str)
        == Some(IDENTITY)
    {
        existing.remove(0);
    }
    existing.retain(|v| v.get("text").and_then(Value::as_str) != Some(""));
    let billing = format!(
        "{BILLING} cc_version={}.{}; cc_entrypoint=cli; cch=00000;",
        identity.version(),
        fingerprint(request, identity.version())
    );
    // The embedding owns cache placement. Do not introduce a shorter head
    // breakpoint in front of an existing caller-selected 1h prefix.
    let control = request.cache_control.as_ref().map_or_else(
        || {
            if existing.iter().any(|v| {
                v.get("cache_control")
                    .and_then(|c| c.get("ttl"))
                    .and_then(Value::as_str)
                    == Some("1h")
            }) {
                json!({"type":"ephemeral","ttl":"1h"})
            } else {
                json!({"type":"ephemeral"})
            }
        },
        |c| serde_json::to_value(c).expect("cache policy serializes"),
    );
    let mut blocks = vec![
        json!({"type":"text","text":billing}),
        json!({"type":"text","text":IDENTITY,"cache_control":control}),
    ];
    blocks.extend(existing);
    request.system = Some(Value::Array(blocks));
}
// JSON.stringify-compatible block order supplies OMP's exact system[0] anchor.
// The checksum is computed over these final bytes before durable effect freezing.
fn stringify(value: &Value, out: &mut String) -> Result<(), ClaudeError> {
    match value {
        Value::Array(a) => {
            out.push('[');
            for (i, v) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                stringify(v, out)?;
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            let mut first = true;
            for key in ["type", "text"]
                .into_iter()
                .filter(|k| map.contains_key(*k))
                .chain(
                    map.keys()
                        .map(String::as_str)
                        .filter(|k| !matches!(*k, "type" | "text")),
                )
            {
                if !first {
                    out.push(',');
                }
                first = false;
                out.push_str(&serde_json::to_string(key)?);
                out.push(':');
                stringify(&map[key], out)?;
            }
            out.push('}');
        }
        _ => out.push_str(&serde_json::to_string(value)?),
    }
    Ok(())
}
fn tool_names(value: &mut Value) {
    if let Some(messages) = value.as_array_mut() {
        for message in messages {
            if let Some(blocks) = message.get_mut("content").and_then(Value::as_array_mut) {
                for block in blocks {
                    if block.get("type").and_then(Value::as_str) == Some("tool_use")
                        && let Some(Value::String(name)) = block.get_mut("name")
                    {
                        *name = prefix(name);
                    }
                    // Only native content blocks, never recursive JSON/input data.
                    if block.get("type").and_then(Value::as_str) == Some("tool_result")
                        && let Some(references) =
                            block.get_mut("content").and_then(Value::as_array_mut)
                    {
                        for reference in references {
                            if reference.get("type").and_then(Value::as_str)
                                == Some("tool_reference")
                                && let Some(Value::String(name)) = reference.get_mut("tool_name")
                            {
                                *name = prefix(name);
                            }
                        }
                    }
                }
            }
        }
    }
}
pub fn body(
    request: &MessagesRequest,
    streaming: bool,
    identity: &SubscriptionIdentity,
    session: &str,
    explicit_cache_tail: bool,
) -> Result<String, ClaudeError> {
    identity.validate()?;
    let mut prepared = request.clone();
    prepare(&mut prepared, identity);
    prepared.validate_cache_control()?;
    // Claude Code never sends top-level automatic caching. After the identity
    // marker has inherited its TTL, move the policy to the final cacheable block.
    if explicit_cache_tail {
        prepared.explicit_cache_tail()?;
    }
    let mut value = serde_json::to_value(&prepared)?;
    value["stream"] = json!(streaming);
    value["metadata"] = identity.metadata(session)?;
    if let Some(tools) = value.get_mut("tools").and_then(Value::as_array_mut) {
        for tool in tools {
            if let Some(Value::String(name)) = tool.get_mut("name") {
                *name = prefix(name);
            }
        }
    } else {
        value["tools"] = json!([]);
    }
    if let Some(Value::String(name)) = value.get_mut("tool_choice").and_then(|v| v.get_mut("name"))
    {
        *name = prefix(name);
    }
    if let Some(messages) = value.get_mut("messages") {
        tool_names(messages);
    }
    let mut wire = String::new();
    stringify(&value, &mut wire)?;
    let mut bytes = wire.into_bytes();
    if let Some(marker) = bytes.windows(MARKER.len()).position(|v| v == MARKER) {
        let start = marker + MARKER.len();
        if let Some(offset) = bytes[start..]
            .windows(PLACEHOLDER.len())
            .position(|v| v == PLACEHOLDER)
            && offset <= 150
        {
            let checksum = format!(
                "{:05x}",
                xxhash_rust::xxh64::xxh64(&bytes, 0x4d65_9218_e32a_3268) & 0xfffff
            );
            bytes[start + offset + 4..start + offset + 9].copy_from_slice(checksum.as_bytes());
        }
    }
    String::from_utf8(bytes)
        .map_err(|_| ClaudeError::Protocol("invalid subscription wire bytes".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn matches_pinned_bun_142_oracle_bytes() {
        // Independent unchanged OMP helpers (Bun >=1.4, as pinned OMP requires)
        // verified these real HTTP captures. Not expectations generated by this port.
        let fixtures: Vec<Value> =
            serde_json::from_str(include_str!("../tests/fixtures/omp-v18.4.4-wire.json")).unwrap();
        let identity = SubscriptionIdentity {
            install_id: Some("synthetic-omp-install".into()),
            account_uuid: Some("11111111-1111-4111-8111-111111111111".into()),
            platform: Some("darwin".into()),
            arch: Some("aarch64".into()),
            ..Default::default()
        };
        for fixture in fixtures {
            let expected = fixture["wire"].as_str().unwrap();
            let mut request: MessagesRequest = serde_json::from_str(expected).unwrap();
            for tool in &mut request.tools {
                if let crate::ClaudeToolSpec::Client(tool) = tool {
                    strip(&mut tool.name);
                }
            }
            assert_eq!(
                body(
                    &request,
                    false,
                    &identity,
                    fixture["session_id"].as_str().unwrap(),
                    true,
                )
                .unwrap(),
                expected
            );
            let before = body(
                &request,
                true,
                &identity,
                fixture["session_id"].as_str().unwrap(),
                true,
            )
            .unwrap();
            prepare(&mut request, &identity);
            prepare(&mut request, &identity);
            assert_eq!(
                body(
                    &request,
                    true,
                    &identity,
                    fixture["session_id"].as_str().unwrap(),
                    true,
                )
                .unwrap(),
                before
            );
        }
    }
    #[test]
    fn keeps_nested_user_data_and_opaque_names_and_prefixes_only_wire_calls() {
        let request: MessagesRequest=serde_json::from_value(json!({"model":"synthetic","max_tokens":16,"messages":[
            {"role":"user","content":[{"type":"text","text":"Caller cch=00000"}]},
            {"role":"assistant","content":[
                {"type":"tool_use","id":"id","name":"_private","input":{"type":"tool_use","name":"user-data"}},
                {"type":"server_tool_use","id":"server","name":"web_search","input":{}},
                {"type":"mcp_tool_use","id":"mcp","name":"read","server_name":"native","input":{}}
            ]}, {"role":"user","content":[{"type":"tool_result","tool_use_id":"search","content":[{"type":"tool_reference","tool_name":"_private"},{"type":"text","text":"native receipt","opaque":{"type":"tool_reference","tool_name":"user-data"}}]}]}],"tools":[],"tool_choice":{"type":"tool","name":"_private"}})).unwrap();
        let wire: Value = serde_json::from_str(
            &body(&request, true, &Default::default(), "stable-session", true).unwrap(),
        )
        .unwrap();
        assert_eq!(wire["messages"][1]["content"][0]["name"], "__private");
        assert_eq!(
            wire["messages"][1]["content"][0]["input"]["name"],
            "user-data"
        );
        assert_eq!(wire["messages"][1]["content"][1]["name"], "web_search");
        assert_eq!(wire["messages"][1]["content"][2]["name"], "read");
        assert_eq!(wire["tool_choice"]["name"], "__private");
        assert_eq!(
            wire["messages"][2]["content"][0]["content"][0]["tool_name"],
            "__private"
        );
        assert_eq!(
            wire["messages"][2]["content"][0]["content"][1]["opaque"]["tool_name"],
            "user-data"
        );
        assert_eq!(
            request.messages[1].content[0].clone(),
            serde_json::from_value(wire["messages"][1]["content"][0].clone())
                .map(|mut b: ContentBlock| {
                    if let ContentBlock::ToolUse { name, .. } = &mut b {
                        strip(name);
                    }
                    b
                })
                .unwrap()
        );
    }
    #[test]
    fn pinned_metadata_and_public_platform_maps() {
        let no_account = SubscriptionIdentity {
            install_id: Some("synthetic-omp-install".into()),
            ..Default::default()
        };
        assert_eq!(
            no_account.metadata("stable-session").unwrap()["user_id"],
            "{\"device_id\":\"7b585e40c3aefdbbb306ed15e0552d8c73e07b56b3a411df2470919e5b79c178\",\"session_id\":\"stable-session\"}"
        );
        for user in [
            r#"{"session_id":"already-bound","opaque":true}"#,
            "user_0000000000000000000000000000000000000000000000000000000000000000_account_11111111-1111-4111-8111-111111111111_session_22222222-2222-4222-8222-222222222222",
        ] {
            let id = SubscriptionIdentity {
                user_id: Some(user.into()),
                ..Default::default()
            };
            assert_eq!(id.metadata("other").unwrap()["user_id"], user);
        }
        for (platform, expected) in [
            ("DARWIN", "MacOS"),
            ("win32", "Windows"),
            ("linux", "Linux"),
            ("freebsd", "FreeBSD"),
            ("Plan9", "Other::plan9"),
        ] {
            assert_eq!(
                SubscriptionIdentity {
                    platform: Some(platform.into()),
                    ..Default::default()
                }
                .os(),
                expected
            );
        }
        for (arch, expected) in [
            ("amd64", "x64"),
            ("aarch64", "arm64"),
            ("386", "x86"),
            ("RiscV64", "other::riscv64"),
        ] {
            assert_eq!(
                SubscriptionIdentity {
                    arch: Some(arch.into()),
                    ..Default::default()
                }
                .arch(),
                expected
            );
        }
    }
}
