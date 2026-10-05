// Copyright 2023-2026 SpaceXAI
// SPDX-License-Identifier: Apache-2.0
// Adapted by Nanocodex: serde_json event boundary and bounded incremental SSE
// decoder replace upstream typed async-openai SamplingEvent plumbing.
// Source: grok-build/crates/codegen/xai-grok-sampler/src/stream/responses.rs
// Pinned revision and license: ../UPSTREAM.md and ../THIRD-PARTY-LICENSES.
use serde_json::Value;

/// Incremental byte framing preserves UTF-8 across network chunk boundaries.
#[derive(Default)]
pub(crate) struct Decoder {
    buffer: Vec<u8>,
}
impl Decoder {
    pub(crate) fn push(&mut self, bytes: &[u8]) -> Result<Vec<Value>, String> {
        self.buffer.extend_from_slice(bytes);
        let mut events = Vec::new();
        while let Some((end, width)) = self
            .buffer
            .windows(2)
            .position(|w| w == b"\n\n")
            .map(|i| (i, 2))
            .into_iter()
            .chain(
                self.buffer
                    .windows(4)
                    .position(|w| w == b"\r\n\r\n")
                    .map(|i| (i, 4)),
            )
            .min_by_key(|p| p.0)
        {
            if end > 8 * 1024 * 1024 {
                return Err("xAI SSE frame exceeded 8 MiB".into());
            }
            let frame = String::from_utf8(self.buffer.drain(..end + width).collect())
                .map_err(|_| "xAI SSE was not UTF-8")?;
            let data = frame
                .lines()
                .filter_map(|l| {
                    l.strip_prefix("data:")
                        .map(|s| s.strip_prefix(' ').unwrap_or(s))
                })
                .collect::<Vec<_>>()
                .join("\n");
            if data.is_empty() || data == "[DONE]" {
                continue;
            }
            events.push(
                serde_json::from_str(&data).map_err(|e| format!("Invalid xAI SSE JSON: {e}"))?,
            );
        }
        if self.buffer.len() > 8 * 1024 * 1024 {
            return Err("xAI SSE frame exceeded 8 MiB".into());
        }
        Ok(events)
    }
}
/// Terminal-state handling adapted from stream_responses_tracked. Incomplete
/// output is rejected here; the sampler separately recognizes empty prompt-limit
/// rejections for bounded compaction. Uncommitted function deltas can never
/// cause a host tool invocation.
pub(crate) fn terminal(event: &Value) -> Result<Option<Value>, String> {
    match event["type"].as_str() {
        Some("response.completed") => {
            let response = &event["response"];
            if response["status"] != "completed" {
                return Err("xAI completed event carried a noncompleted status".into());
            }
            if !response["output"].is_array() {
                return Err("xAI completed response omitted output".into());
            }
            Ok(Some(response.clone()))
        }
        Some("response.incomplete") => Err(format!(
            "xAI response incomplete: {}",
            event["response"]["incomplete_details"]
        )),
        Some("response.failed") => Err(format!(
            "xAI response failed: {}",
            event["response"]["error"]
        )),
        Some("error" | "response.error") => Err(format!("xAI stream error: {}", event["message"])),
        _ => Ok(None),
    }
}
