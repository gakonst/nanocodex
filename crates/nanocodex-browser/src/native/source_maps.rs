use std::{
    collections::HashMap,
    sync::{Arc, Mutex as StdMutex},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use chromiumoxide::{
    Page,
    cdp::{
        browser_protocol::{
            io::{CloseParams, ReadParams},
            network::{LoadNetworkResourceOptions, LoadNetworkResourceParams},
            page::FrameId,
        },
        js_protocol::{
            debugger::{EnableParams, EventScriptParsed},
            runtime::StackTrace,
        },
    },
};
use futures_util::StreamExt;
use sourcemap::SourceMap;
use tracing::warn;
use url::Url;

use crate::{
    BrowserConsoleEntry, BrowserPageError, BrowserPerformanceInsight, BrowserPerformanceSource,
    BrowserSourceLocation, BrowserStackFrame, trace_serialized,
};

use super::BrowserError;

const MAX_SOURCE_MAP_BYTES: usize = 20 * 1024 * 1024;
const MAX_SOURCE_MAP_ENCODED_BYTES: usize = MAX_SOURCE_MAP_BYTES.div_ceil(3) * 4;
const MAX_SOURCE_MAPS: usize = 256;
const MAX_SCRIPT_URLS: usize = 4_096;
const MAX_STACK_FRAMES: usize = 64;
const MAX_STACK_FIELD_BYTES: usize = 4 * 1024;

#[derive(Clone, Default)]
pub(super) struct SourceMaps {
    maps: Arc<StdMutex<HashMap<String, Arc<SourceMap>>>>,
    script_urls: Arc<StdMutex<HashMap<String, String>>>,
}

impl SourceMaps {
    pub(super) fn symbolicate_console(
        &self,
        entries: &mut [BrowserConsoleEntry],
    ) -> Result<(), BrowserError> {
        for entry in entries {
            self.symbolicate(&mut entry.stack)?;
        }
        Ok(())
    }

    pub(super) fn symbolicate_errors(
        &self,
        errors: &mut [BrowserPageError],
    ) -> Result<(), BrowserError> {
        for error in errors {
            self.symbolicate(&mut error.stack)?;
        }
        Ok(())
    }

    pub(super) fn symbolicate_performance(
        &self,
        insights: &mut [BrowserPerformanceInsight],
    ) -> Result<(), BrowserError> {
        let maps = self
            .maps
            .lock()
            .map_err(|_| BrowserError::SourceMapsUnavailable)?;
        for insight in insights {
            let source = match insight {
                BrowserPerformanceInsight::LongTask { source, .. }
                | BrowserPerformanceInsight::ForcedReflow { source, .. } => source.as_mut(),
                BrowserPerformanceInsight::SlowCssSelector { .. }
                | BrowserPerformanceInsight::LayoutShift { .. }
                | BrowserPerformanceInsight::LargestContentfulPaint { .. }
                | BrowserPerformanceInsight::RenderBlockingResource { .. }
                | BrowserPerformanceInsight::NetworkDependencyChain { .. }
                | BrowserPerformanceInsight::DuplicateJavaScript { .. }
                | BrowserPerformanceInsight::CacheOpportunity { .. }
                | BrowserPerformanceInsight::DomSize { .. }
                | BrowserPerformanceInsight::ThirdParty { .. } => None,
            };
            let Some(source) = source else {
                continue;
            };
            symbolicate_performance_source(&maps, source);
        }
        Ok(())
    }

    pub(super) fn debugger_location(
        &self,
        script_id: &str,
        line_number: i64,
        column_number: i64,
        function_name: Option<String>,
    ) -> Result<(BrowserSourceLocation, Option<BrowserSourceLocation>), BrowserError> {
        let url = self
            .script_urls
            .lock()
            .map_err(|_| BrowserError::SourceMapsUnavailable)?
            .get(script_id)
            .cloned()
            .unwrap_or_default();
        let generated = BrowserSourceLocation {
            url,
            line_number: u64::try_from(line_number)
                .unwrap_or_default()
                .saturating_add(1),
            column_number: u64::try_from(column_number)
                .unwrap_or_default()
                .saturating_add(1),
            function_name,
        };
        let original = self
            .maps
            .lock()
            .map_err(|_| BrowserError::SourceMapsUnavailable)?
            .get(script_id)
            .and_then(|map| {
                let line = u32::try_from(line_number).ok()?;
                let column = u32::try_from(column_number).ok()?;
                let token = map.lookup_token(line, column)?;
                Some(BrowserSourceLocation {
                    url: token.get_source()?.to_owned(),
                    line_number: u64::from(token.get_src_line()) + 1,
                    column_number: u64::from(token.get_src_col()) + 1,
                    function_name: token
                        .get_name()
                        .map(str::to_owned)
                        .or_else(|| generated.function_name.clone()),
                })
            });
        Ok((generated, original))
    }

    fn symbolicate(&self, frames: &mut [BrowserStackFrame]) -> Result<(), BrowserError> {
        let maps = self
            .maps
            .lock()
            .map_err(|_| BrowserError::SourceMapsUnavailable)?;
        for frame in frames {
            let Some(map) = maps.get(&frame.script_id) else {
                continue;
            };
            let generated_line = frame.generated.line_number.saturating_sub(1);
            let generated_column = frame.generated.column_number.saturating_sub(1);
            let (Ok(line), Ok(column)) = (
                u32::try_from(generated_line),
                u32::try_from(generated_column),
            ) else {
                continue;
            };
            let Some(token) = map.lookup_token(line, column) else {
                continue;
            };
            let Some(source) = token.get_source() else {
                continue;
            };
            frame.original = Some(BrowserSourceLocation {
                url: source.to_owned(),
                line_number: u64::from(token.get_src_line()) + 1,
                column_number: u64::from(token.get_src_col()) + 1,
                function_name: token.get_name().map(str::to_owned),
            });
        }
        Ok(())
    }
}

fn symbolicate_performance_source(
    maps: &HashMap<String, Arc<SourceMap>>,
    source: &mut BrowserPerformanceSource,
) {
    let (Some(script_id), Some(line), Some(column)) = (
        source.script_id.as_deref(),
        source.line_number,
        source.column_number,
    ) else {
        return;
    };
    let (Ok(line), Ok(column)) = (
        u32::try_from(line.saturating_sub(1)),
        u32::try_from(column.saturating_sub(1)),
    ) else {
        return;
    };
    let Some(token) = maps
        .get(script_id)
        .and_then(|map| map.lookup_token(line, column))
    else {
        return;
    };
    let Some(url) = token.get_source() else {
        return;
    };
    source.original = Some(BrowserSourceLocation {
        url: url.to_owned(),
        line_number: u64::from(token.get_src_line()) + 1,
        column_number: u64::from(token.get_src_col()) + 1,
        function_name: token
            .get_name()
            .map(str::to_owned)
            .or_else(|| source.function_name.clone()),
    });
}

pub(super) async fn start(
    page: &Page,
) -> Result<(SourceMaps, tokio::task::JoinHandle<()>), BrowserError> {
    let mut events = page.event_listener::<EventScriptParsed>().await?;
    page.execute(EnableParams::default()).await?;
    let maps = SourceMaps::default();
    let task_maps = maps.clone();
    let page = page.clone();
    let task = tokio::spawn(async move {
        while let Some(event) = events.next().await {
            trace_serialized("devtools.Debugger.scriptParsed", event.as_ref());
            let script_id = event.script_id.as_ref().to_owned();
            {
                let Ok(mut script_urls) = task_maps.script_urls.lock() else {
                    break;
                };
                if script_urls.len() < MAX_SCRIPT_URLS || script_urls.contains_key(&script_id) {
                    script_urls.insert(
                        script_id.clone(),
                        bounded_string(&event.url, MAX_STACK_FIELD_BYTES),
                    );
                }
            }
            let Some(source_map_url) = event.source_map_url.as_deref() else {
                continue;
            };
            match load_source_map(&page, &event.url, source_map_url, event_frame_id(&event)).await {
                Ok(Some(map)) => {
                    let Ok(mut maps) = task_maps.maps.lock() else {
                        break;
                    };
                    if maps.len() < MAX_SOURCE_MAPS || maps.contains_key(&script_id) {
                        maps.insert(script_id, Arc::new(map));
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    warn!(
                        target: "nanocodex_browser",
                        script_url = event.url,
                        source_map_url,
                        %error,
                        "could not load JavaScript source map"
                    );
                }
            }
        }
    });
    Ok((maps, task))
}

pub(super) fn stack_frames(trace: Option<&StackTrace>) -> Vec<BrowserStackFrame> {
    let mut frames = Vec::new();
    if let Some(trace) = trace {
        collect_frames(trace, None, &mut frames);
    }
    frames
}

fn collect_frames(
    trace: &StackTrace,
    async_parent: Option<&str>,
    frames: &mut Vec<BrowserStackFrame>,
) {
    let available = MAX_STACK_FRAMES.saturating_sub(frames.len());
    for (index, frame) in trace.call_frames.iter().take(available).enumerate() {
        frames.push(BrowserStackFrame {
            script_id: bounded_string(frame.script_id.as_ref(), MAX_STACK_FIELD_BYTES),
            function_name: bounded_string(&frame.function_name, MAX_STACK_FIELD_BYTES),
            generated: BrowserSourceLocation {
                url: bounded_string(&frame.url, MAX_STACK_FIELD_BYTES),
                line_number: u64::try_from(frame.line_number)
                    .unwrap_or_default()
                    .saturating_add(1),
                column_number: u64::try_from(frame.column_number)
                    .unwrap_or_default()
                    .saturating_add(1),
                function_name: Some(bounded_string(&frame.function_name, MAX_STACK_FIELD_BYTES)),
            },
            original: None,
            async_parent: (index == 0)
                .then(|| async_parent.map(|parent| bounded_string(parent, MAX_STACK_FIELD_BYTES)))
                .flatten(),
        });
    }
    if frames.len() < MAX_STACK_FRAMES
        && let Some(parent) = trace.parent.as_deref()
    {
        collect_frames(
            parent,
            Some(trace.description.as_deref().unwrap_or("async")),
            frames,
        );
    }
}

async fn load_source_map(
    page: &Page,
    script_url: &str,
    source_map_url: &str,
    frame_id: Option<FrameId>,
) -> Result<Option<SourceMap>, BrowserError> {
    let bytes = if source_map_url.starts_with("data:") {
        decode_data_url(source_map_url)?
    } else {
        let Some(url) = resolve_same_origin(script_url, source_map_url) else {
            return Ok(None);
        };
        load_network_resource(page, url, frame_id).await?
    };
    if bytes.len() > MAX_SOURCE_MAP_BYTES {
        return Err(BrowserError::SourceMapTooLarge {
            bytes: bytes.len(),
            maximum: MAX_SOURCE_MAP_BYTES,
        });
    }
    Ok(Some(
        SourceMap::from_slice(&bytes).map_err(BrowserError::SourceMapParse)?,
    ))
}

fn decode_data_url(value: &str) -> Result<Vec<u8>, BrowserError> {
    let (metadata, payload) = value
        .split_once(',')
        .ok_or(BrowserError::InvalidSourceMapDataUrl)?;
    if !metadata.ends_with(";base64") {
        return Err(BrowserError::UnsupportedSourceMapDataUrl);
    }
    if payload.len() > MAX_SOURCE_MAP_ENCODED_BYTES {
        return Err(BrowserError::SourceMapTooLarge {
            bytes: payload.len().saturating_mul(3) / 4,
            maximum: MAX_SOURCE_MAP_BYTES,
        });
    }
    STANDARD
        .decode(payload)
        .map_err(BrowserError::SourceMapBase64)
}

fn bounded_string(value: &str, maximum: usize) -> String {
    if value.len() <= maximum {
        return value.to_owned();
    }
    let mut boundary = maximum;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value[..boundary].to_owned()
}

fn resolve_same_origin(script_url: &str, source_map_url: &str) -> Option<Url> {
    let script = Url::parse(script_url).ok()?;
    let source_map = script.join(source_map_url).ok()?;
    (source_map.origin() == script.origin()).then_some(source_map)
}

fn event_frame_id(event: &EventScriptParsed) -> Option<FrameId> {
    event
        .execution_context_aux_data
        .as_ref()
        .and_then(|data| data.get("frameId"))
        .and_then(serde_json::Value::as_str)
        .map(|frame_id| FrameId::new(frame_id.to_owned()))
}

async fn load_network_resource(
    page: &Page,
    url: Url,
    frame_id: Option<FrameId>,
) -> Result<Vec<u8>, BrowserError> {
    let mut params = LoadNetworkResourceParams::new(
        url.to_string(),
        LoadNetworkResourceOptions::new(false, true),
    );
    params.frame_id = frame_id;
    let response = page.execute(params).await?;
    if !response.resource.success {
        return Err(BrowserError::SourceMapNetwork {
            url,
            message: response
                .resource
                .net_error_name
                .clone()
                .unwrap_or_else(|| "network resource load failed".to_owned()),
        });
    }
    let stream =
        response
            .resource
            .stream
            .clone()
            .ok_or_else(|| BrowserError::SourceMapNetwork {
                url: url.clone(),
                message: "network resource did not return a stream".to_owned(),
            })?;
    let mut bytes = Vec::new();
    loop {
        let mut params = ReadParams::new(stream.clone());
        params.size = Some(256 * 1024);
        let response = page.execute(params).await?;
        if response.base64_encoded.unwrap_or(false) {
            bytes.extend(
                STANDARD
                    .decode(response.data.as_bytes())
                    .map_err(BrowserError::SourceMapBase64)?,
            );
        } else {
            bytes.extend_from_slice(response.data.as_bytes());
        }
        if bytes.len() > MAX_SOURCE_MAP_BYTES || response.eof {
            break;
        }
    }
    let _ = page.execute(CloseParams::new(stream)).await;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use sourcemap::SourceMap;

    use crate::{BrowserSourceLocation, BrowserStackFrame};

    use super::SourceMaps;

    #[test]
    fn source_maps_preserve_generated_and_add_original_locations() {
        let source_map = SourceMap::from_slice(
            br#"{"version":3,"file":"app.js","sources":["src/app.ts"],"names":["fail"],"mappings":"AAAAA"}"#,
        )
        .expect("valid source map");
        let maps = SourceMaps::default();
        maps.maps
            .lock()
            .expect("source-map registry")
            .insert("script-1".to_owned(), Arc::new(source_map));
        let mut frames = vec![BrowserStackFrame {
            script_id: "script-1".to_owned(),
            function_name: "fail".to_owned(),
            generated: BrowserSourceLocation {
                url: "http://fixture/app.js".to_owned(),
                line_number: 1,
                column_number: 42,
                function_name: Some("fail".to_owned()),
            },
            original: None,
            async_parent: None,
        }];

        maps.symbolicate(&mut frames).expect("symbolication");
        assert_eq!(frames[0].generated.url, "http://fixture/app.js");
        assert_eq!(
            frames[0]
                .original
                .as_ref()
                .map(|location| location.url.as_str()),
            Some("src/app.ts")
        );
        assert_eq!(
            frames[0]
                .original
                .as_ref()
                .and_then(|location| location.function_name.as_deref()),
            Some("fail")
        );
    }
}
