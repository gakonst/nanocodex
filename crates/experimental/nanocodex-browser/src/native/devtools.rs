use std::{
    collections::HashMap,
    sync::{Arc, Mutex as StdMutex},
};

use chromiumoxide::{
    Page,
    cdp::{
        browser_protocol::{
            css::{
                CssProperty, CssStyle, EnableParams as CssEnableParams, EventStyleSheetAdded,
                ForcePseudoStateParams, GetMatchedStylesForNodeParams,
                GetMatchedStylesForNodeReturns, RuleMatch, SourceRange,
            },
            dom::{
                EnableParams as DomEnableParams, EventDocumentUpdated, GetDocumentParams, NodeId,
                RequestNodeParams,
            },
            dom_debugger::GetEventListenersParams,
        },
        js_protocol::{
            debugger::{
                EnableParams as DebuggerEnableParams, EventPaused, RemoveBreakpointParams,
                ResumeParams, SetBreakpointByUrlParams, SetPauseOnExceptionsParams,
                SetPauseOnExceptionsState, StepIntoParams, StepOutParams, StepOverParams,
            },
            runtime::{EvaluateParams, ReleaseObjectParams},
        },
    },
};
use futures_util::StreamExt;
use tokio::task::JoinHandle;
use tracing::warn;

use crate::{
    BrowserBreakpoint, BrowserCssProperty, BrowserCssRule, BrowserCssSourceRange,
    BrowserDebuggerFrame, BrowserDebuggerPause, BrowserDebuggerScope, BrowserEventListener,
    BrowserMatchedStyles, BrowserPauseOnExceptions, BrowserPseudoClass, BrowserSourceLocation,
    BrowserStorageReport, trace_serialized,
};

use super::{BrowserError, ElementTarget, element_script, evaluate_typed, source_maps::SourceMaps};

const MAX_STYLE_SHEETS: usize = 4_096;
const MAX_DEBUGGER_FRAMES: usize = 128;
const MAX_STYLE_SHEET_URL_CHARS: usize = 4_096;

#[derive(Clone, Default)]
pub(super) struct DevtoolsDiagnostics {
    style_sheets: Arc<StdMutex<HashMap<String, String>>>,
    pause: Arc<StdMutex<PauseState>>,
    dom_document_requested: Arc<StdMutex<bool>>,
}

#[derive(Default)]
struct PauseState {
    next_sequence: u64,
    latest: Option<BrowserDebuggerPause>,
}

pub(super) async fn start(
    page: &Page,
    source_maps: SourceMaps,
) -> Result<(DevtoolsDiagnostics, Vec<JoinHandle<()>>), BrowserError> {
    let mut style_events = page.event_listener::<EventStyleSheetAdded>().await?;
    let mut pause_events = page.event_listener::<EventPaused>().await?;
    let mut document_events = page.event_listener::<EventDocumentUpdated>().await?;
    page.execute(DomEnableParams::default()).await?;
    page.execute(CssEnableParams::default()).await?;
    page.execute(DebuggerEnableParams::default()).await?;

    let diagnostics = DevtoolsDiagnostics::default();
    let style_sheets = Arc::clone(&diagnostics.style_sheets);
    let styles = tokio::spawn(async move {
        while let Some(event) = style_events.next().await {
            trace_serialized("devtools.CSS.styleSheetAdded", event.as_ref());
            let Ok(mut style_sheets) = style_sheets.lock() else {
                break;
            };
            let style_sheet_id = event.header.style_sheet_id.as_ref();
            if style_sheets.len() < MAX_STYLE_SHEETS || style_sheets.contains_key(style_sheet_id) {
                style_sheets.insert(
                    style_sheet_id.to_owned(),
                    event
                        .header
                        .source_url
                        .chars()
                        .take(MAX_STYLE_SHEET_URL_CHARS)
                        .collect(),
                );
            }
        }
    });

    let pause_state = Arc::clone(&diagnostics.pause);
    let pauses = tokio::spawn(async move {
        while let Some(event) = pause_events.next().await {
            trace_serialized("devtools.Debugger.paused", event.as_ref());
            let frames = event
                .call_frames
                .iter()
                .take(MAX_DEBUGGER_FRAMES)
                .map(|frame| debugger_frame(frame, &source_maps))
                .collect::<Vec<_>>();
            let Ok(mut pause_state) = pause_state.lock() else {
                break;
            };
            let sequence = pause_state.next_sequence;
            pause_state.next_sequence = pause_state.next_sequence.saturating_add(1);
            pause_state.latest = Some(BrowserDebuggerPause {
                sequence,
                reason: event.reason.as_ref().to_owned(),
                hit_breakpoints: event.hit_breakpoints.clone().unwrap_or_default(),
                frames,
            });
        }
    });
    let document_requested = Arc::clone(&diagnostics.dom_document_requested);
    let documents = tokio::spawn(async move {
        while let Some(event) = document_events.next().await {
            trace_serialized("devtools.DOM.documentUpdated", event.as_ref());
            let Ok(mut requested) = document_requested.lock() else {
                break;
            };
            *requested = false;
        }
    });
    Ok((diagnostics, vec![styles, pauses, documents]))
}

impl DevtoolsDiagnostics {
    pub(super) fn latest_pause(&self) -> Result<Option<BrowserDebuggerPause>, BrowserError> {
        Ok(self
            .pause
            .lock()
            .map_err(|_| BrowserError::DiagnosticsUnavailable)?
            .latest
            .clone())
    }

    pub(super) fn clear_pause(&self) -> Result<(), BrowserError> {
        self.pause
            .lock()
            .map_err(|_| BrowserError::DiagnosticsUnavailable)?
            .latest = None;
        Ok(())
    }

    async fn ensure_document(&self, page: &Page) -> Result<(), BrowserError> {
        if *self
            .dom_document_requested
            .lock()
            .map_err(|_| BrowserError::DiagnosticsUnavailable)?
        {
            return Ok(());
        }
        page.execute(GetDocumentParams::default()).await?;
        *self
            .dom_document_requested
            .lock()
            .map_err(|_| BrowserError::DiagnosticsUnavailable)? = true;
        Ok(())
    }

    fn source_url(&self, style_sheet_id: &str) -> Result<Option<String>, BrowserError> {
        Ok(self
            .style_sheets
            .lock()
            .map_err(|_| BrowserError::DiagnosticsUnavailable)?
            .get(style_sheet_id)
            .filter(|url| !url.is_empty())
            .cloned())
    }
}

pub(super) async fn matched_styles(
    page: &Page,
    target: &ElementTarget,
    diagnostics: &DevtoolsDiagnostics,
) -> Result<BrowserMatchedStyles, BrowserError> {
    let node_id = resolve_node(page, target, diagnostics).await?;
    let styles = page
        .execute(GetMatchedStylesForNodeParams::new(node_id))
        .await?;
    map_matched_styles(&styles, diagnostics)
}

pub(super) async fn force_pseudo_state(
    page: &Page,
    target: &ElementTarget,
    diagnostics: &DevtoolsDiagnostics,
    pseudo_classes: Vec<BrowserPseudoClass>,
) -> Result<(), BrowserError> {
    let node_id = resolve_node(page, target, diagnostics).await?;
    page.execute(ForcePseudoStateParams::new(
        node_id,
        pseudo_classes
            .into_iter()
            .map(|pseudo| pseudo.as_css().to_owned())
            .collect(),
    ))
    .await?;
    Ok(())
}

pub(super) async fn event_listeners(
    page: &Page,
    target: &ElementTarget,
    depth: u8,
    pierce: bool,
) -> Result<Vec<BrowserEventListener>, BrowserError> {
    let object_id = resolve_object(page, target).await?;
    let mut params = GetEventListenersParams::new(object_id.clone());
    params.depth = Some(i64::from(depth));
    params.pierce = Some(pierce);
    let listeners = page.execute(params).await;
    let _ = page.execute(ReleaseObjectParams::new(object_id)).await;
    Ok(listeners?
        .listeners
        .iter()
        .map(|listener| BrowserEventListener {
            event_type: listener.r#type.clone(),
            use_capture: listener.use_capture,
            passive: listener.passive,
            once: listener.once,
            script_id: listener.script_id.as_ref().to_owned(),
            line_number: u64::try_from(listener.line_number)
                .unwrap_or_default()
                .saturating_add(1),
            column_number: u64::try_from(listener.column_number)
                .unwrap_or_default()
                .saturating_add(1),
        })
        .collect())
}

pub(super) async fn set_pause_on_exceptions(
    page: &Page,
    state: BrowserPauseOnExceptions,
) -> Result<(), BrowserError> {
    let state = match state {
        BrowserPauseOnExceptions::None => SetPauseOnExceptionsState::None,
        BrowserPauseOnExceptions::Uncaught => SetPauseOnExceptionsState::Uncaught,
        BrowserPauseOnExceptions::All => SetPauseOnExceptionsState::All,
    };
    page.execute(SetPauseOnExceptionsParams::new(state)).await?;
    Ok(())
}

pub(super) async fn set_breakpoint(
    page: &Page,
    source_maps: &SourceMaps,
    url: String,
    line_number: u64,
    column_number: u64,
    condition: Option<String>,
) -> Result<BrowserBreakpoint, BrowserError> {
    let zero_line = line_number
        .checked_sub(1)
        .and_then(|line| i64::try_from(line).ok())
        .ok_or(BrowserError::InvalidSourceLocation {
            line_number,
            column_number,
        })?;
    let zero_column = column_number
        .checked_sub(1)
        .and_then(|column| i64::try_from(column).ok())
        .ok_or(BrowserError::InvalidSourceLocation {
            line_number,
            column_number,
        })?;
    let mut params = SetBreakpointByUrlParams::new(zero_line);
    params.url = Some(url.clone());
    params.column_number = Some(zero_column);
    params.condition = condition;
    let breakpoint = page.execute(params).await?;
    let resolved_locations = breakpoint
        .locations
        .iter()
        .map(|location| {
            let (mut generated, _) = source_maps.debugger_location(
                location.script_id.as_ref(),
                location.line_number,
                location.column_number.unwrap_or_default(),
                None,
            )?;
            if generated.url.is_empty() {
                generated.url.clone_from(&url);
            }
            Ok(generated)
        })
        .collect::<Result<Vec<_>, BrowserError>>()?;
    Ok(BrowserBreakpoint {
        breakpoint_id: breakpoint.breakpoint_id.as_ref().to_owned(),
        url,
        line_number,
        column_number,
        resolved_locations,
    })
}

pub(super) async fn remove_breakpoint(
    page: &Page,
    breakpoint_id: String,
) -> Result<(), BrowserError> {
    page.execute(RemoveBreakpointParams::new(breakpoint_id))
        .await?;
    Ok(())
}

pub(super) async fn resume(page: &Page) -> Result<(), BrowserError> {
    page.execute(ResumeParams::default()).await?;
    Ok(())
}

pub(super) async fn step_over(page: &Page) -> Result<(), BrowserError> {
    page.execute(StepOverParams::default()).await?;
    Ok(())
}

pub(super) async fn step_into(page: &Page) -> Result<(), BrowserError> {
    page.execute(StepIntoParams::default()).await?;
    Ok(())
}

pub(super) async fn step_out(page: &Page) -> Result<(), BrowserError> {
    page.execute(StepOutParams::default()).await?;
    Ok(())
}

pub(super) async fn storage_report(page: &Page) -> Result<BrowserStorageReport, BrowserError> {
    evaluate_typed(page, STORAGE_REPORT_SCRIPT).await
}

async fn resolve_node(
    page: &Page,
    target: &ElementTarget,
    diagnostics: &DevtoolsDiagnostics,
) -> Result<NodeId, BrowserError> {
    // CSS domain commands consume frontend `NodeId`s. Chrome only retains
    // those IDs after the client has requested the current document.
    diagnostics.ensure_document(page).await?;
    let object_id = resolve_object(page, target).await?;
    let node = page
        .execute(RequestNodeParams::new(object_id.clone()))
        .await;
    let _ = page.execute(ReleaseObjectParams::new(object_id)).await;
    Ok(node?.node_id)
}

async fn resolve_object(
    page: &Page,
    target: &ElementTarget,
) -> Result<chromiumoxide::cdp::js_protocol::runtime::RemoteObjectId, BrowserError> {
    let mut params = EvaluateParams::new(element_script(&target.query, "return element;")?);
    params.context_id = target.context_id;
    params.return_by_value = Some(false);
    params.await_promise = Some(true);
    let result = page.evaluate_expression(params).await?;
    result
        .object()
        .object_id
        .clone()
        .ok_or_else(|| BrowserError::Configuration {
            message: "element did not produce a remote object".to_owned(),
        })
}

fn map_matched_styles(
    styles: &GetMatchedStylesForNodeReturns,
    diagnostics: &DevtoolsDiagnostics,
) -> Result<BrowserMatchedStyles, BrowserError> {
    let mut rules = styles
        .matched_css_rules
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|rule| map_rule(rule, diagnostics, None, None))
        .collect::<Result<Vec<_>, _>>()?;
    for (depth, inherited) in styles
        .inherited
        .as_deref()
        .unwrap_or_default()
        .iter()
        .enumerate()
    {
        let depth = u32::try_from(depth).unwrap_or(u32::MAX);
        if let Some(inline) = &inherited.inline_style {
            rules.push(BrowserCssRule {
                selector: "<inline inherited style>".to_owned(),
                matching_selectors: Vec::new(),
                origin: "inline".to_owned(),
                style_sheet_id: inline
                    .style_sheet_id
                    .as_ref()
                    .map(|id| id.as_ref().to_owned()),
                source_url: None,
                range: inline.range.as_ref().map(map_range),
                properties: map_properties(inline),
                inherited_from: Some(depth),
                pseudo: None,
            });
        }
        rules.extend(
            inherited
                .matched_css_rules
                .iter()
                .map(|rule| map_rule(rule, diagnostics, Some(depth), None))
                .collect::<Result<Vec<_>, _>>()?,
        );
    }
    for pseudo in styles.pseudo_elements.as_deref().unwrap_or_default() {
        let pseudo_name = pseudo.pseudo_identifier.as_ref().map_or_else(
            || pseudo.pseudo_type.as_ref().to_owned(),
            |identifier| format!("{}({identifier})", pseudo.pseudo_type.as_ref()),
        );
        rules.extend(
            pseudo
                .matches
                .iter()
                .map(|rule| map_rule(rule, diagnostics, None, Some(pseudo_name.clone())))
                .collect::<Result<Vec<_>, _>>()?,
        );
    }
    Ok(BrowserMatchedStyles {
        inline_style: styles
            .inline_style
            .as_ref()
            .map_or_else(Vec::new, map_properties),
        attributes_style: styles
            .attributes_style
            .as_ref()
            .map_or_else(Vec::new, map_properties),
        rules,
    })
}

fn map_rule(
    matched: &RuleMatch,
    diagnostics: &DevtoolsDiagnostics,
    inherited_from: Option<u32>,
    pseudo: Option<String>,
) -> Result<BrowserCssRule, BrowserError> {
    let style_sheet_id = matched
        .rule
        .style_sheet_id
        .as_ref()
        .map(|id| id.as_ref().to_owned());
    let source_url = style_sheet_id
        .as_deref()
        .map(|id| diagnostics.source_url(id))
        .transpose()?
        .flatten();
    let matching_selectors = matched
        .matching_selectors
        .iter()
        .filter_map(|index| usize::try_from(*index).ok())
        .filter_map(|index| matched.rule.selector_list.selectors.get(index))
        .map(|selector| selector.text.clone())
        .collect();
    let range = matched
        .matching_selectors
        .first()
        .and_then(|index| usize::try_from(*index).ok())
        .and_then(|index| matched.rule.selector_list.selectors.get(index))
        .and_then(|selector| selector.range.as_ref())
        .or(matched.rule.style.range.as_ref())
        .map(map_range);
    Ok(BrowserCssRule {
        selector: matched.rule.selector_list.text.clone(),
        matching_selectors,
        origin: matched.rule.origin.as_ref().to_owned(),
        style_sheet_id,
        source_url,
        range,
        properties: map_properties(&matched.rule.style),
        inherited_from,
        pseudo,
    })
}

fn map_properties(style: &CssStyle) -> Vec<BrowserCssProperty> {
    style.css_properties.iter().map(map_property).collect()
}

fn map_property(property: &CssProperty) -> BrowserCssProperty {
    BrowserCssProperty {
        name: property.name.clone(),
        value: property.value.clone(),
        important: property.important.unwrap_or(false),
        implicit: property.implicit.unwrap_or(false),
        parsed: property.parsed_ok.unwrap_or(true),
        disabled: property.disabled.unwrap_or(false),
        range: property.range.as_ref().map(map_range),
    }
}

const fn map_range(range: &SourceRange) -> BrowserCssSourceRange {
    BrowserCssSourceRange {
        start_line: range.start_line,
        start_column: range.start_column,
        end_line: range.end_line,
        end_column: range.end_column,
    }
}

fn debugger_frame(
    frame: &chromiumoxide::cdp::js_protocol::debugger::CallFrame,
    source_maps: &SourceMaps,
) -> BrowserDebuggerFrame {
    let (location, original) = match source_maps.debugger_location(
        frame.location.script_id.as_ref(),
        frame.location.line_number,
        frame.location.column_number.unwrap_or_default(),
        Some(frame.function_name.clone()),
    ) {
        Ok(location) => location,
        Err(error) => {
            warn!(
                target: "nanocodex_browser",
                %error,
                "could not resolve debugger source location"
            );
            (
                BrowserSourceLocation {
                    url: String::new(),
                    line_number: u64::try_from(frame.location.line_number)
                        .unwrap_or_default()
                        .saturating_add(1),
                    column_number: u64::try_from(frame.location.column_number.unwrap_or_default())
                        .unwrap_or_default()
                        .saturating_add(1),
                    function_name: Some(frame.function_name.clone()),
                },
                None,
            )
        }
    };
    BrowserDebuggerFrame {
        function_name: frame.function_name.clone(),
        location,
        original,
        scopes: frame
            .scope_chain
            .iter()
            .map(|scope| BrowserDebuggerScope {
                kind: scope.r#type.as_ref().to_owned(),
                name: scope.name.clone(),
                object_type: scope.object.r#type.as_ref().to_owned(),
                object_description: scope.object.description.clone(),
            })
            .collect(),
    }
}

const STORAGE_REPORT_SCRIPT: &str = r#"(
async () => {
  const serviceWorkers = [];
  if ("serviceWorker" in navigator) {
    for (const registration of await navigator.serviceWorker.getRegistrations()) {
      const worker = registration.active ?? registration.waiting ?? registration.installing;
      serviceWorkers.push({
        scope: registration.scope,
        scriptUrl: worker?.scriptURL ?? null,
        state: worker?.state ?? null
      });
    }
  }
  const cacheReports = [];
  if ("caches" in globalThis) {
    for (const name of await caches.keys()) {
      const cache = await caches.open(name);
      const entries = (await cache.keys()).map(request => ({
        method: request.method,
        url: request.url
      }));
      cacheReports.push({ name, entries });
    }
  }
  const indexedDb = typeof indexedDB.databases === "function"
    ? (await indexedDB.databases()).map(database => ({
        name: database.name ?? "",
        version: database.version ?? 0
      }))
    : [];
  return {
    origin: location.origin,
    serviceWorkers,
    caches: cacheReports,
    indexedDb,
    localStorageKeys: Object.keys(localStorage).sort(),
    sessionStorageKeys: Object.keys(sessionStorage).sort()
  };
}
)()"#;
