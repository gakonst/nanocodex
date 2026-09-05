use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex as StdMutex},
    time::{Duration, Instant},
};

use chromiumoxide::{
    Command, Method, Page,
    cdp::{
        browser_protocol::{
            dom::GetContentQuadsParams,
            input::{
                DispatchDragEventParams, DispatchDragEventType, DispatchKeyEventParams,
                DispatchKeyEventType, DispatchMouseEventParams, DispatchMouseEventType,
                DispatchTouchEventParams, DispatchTouchEventReturns, DispatchTouchEventType,
                DragData, DragDataItem, EventDragIntercepted, InsertTextParams,
                MouseButton as CdpMouseButton, SetInterceptDragsParams, TouchPoint,
            },
        },
        js_protocol::runtime::{EvaluateParams, ReleaseObjectParams},
    },
    keys,
    layout::Point,
    types::MethodId,
};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::time::{sleep, timeout};

use crate::{
    BrowserActionNetwork, BrowserClickOptions, BrowserKeyModifier, BrowserLoadState,
    BrowserMouseButton, BrowserSnapshotMatch, BrowserWaitForSelectorState, trace_serialized,
};

use super::{
    BrowserError, DEFAULT_WAIT_TIMEOUT, Diagnostics, ElementQuery, ElementTarget, NetworkEntry,
    evaluate_typed, evaluate_typed_in_context, query_resolver_prelude,
};

const ACTION_SETTLE: Duration = Duration::from_millis(100);
const ACTION_NETWORK_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(25);

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TouchEventParams {
    #[serde(rename = "type")]
    event_type: DispatchTouchEventType,
    touch_points: Vec<TouchPoint>,
}

impl TouchEventParams {
    const fn new(event_type: DispatchTouchEventType, touch_points: Vec<TouchPoint>) -> Self {
        Self {
            event_type,
            touch_points,
        }
    }
}

impl Method for TouchEventParams {
    fn identifier(&self) -> MethodId {
        DispatchTouchEventParams::IDENTIFIER.into()
    }
}

impl Command for TouchEventParams {
    type Response = DispatchTouchEventReturns;
}

#[derive(Clone, Copy)]
pub(super) enum Actionability {
    Attached,
    Enabled,
    Editable,
    Pointer,
}

impl Actionability {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Attached => "attached",
            Self::Enabled => "enabled",
            Self::Editable => "editable",
            Self::Pointer => "pointer",
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ActionabilityWire {
    state: String,
    reason: Option<String>,
    count: usize,
}

pub(super) async fn actionable_point(
    page: &Page,
    target: &ElementTarget,
    requirement: Actionability,
) -> Result<Point, BrowserError> {
    wait_until_actionable(page, target, requirement).await?;
    let expression = element_expression(&target.query)?;
    let mut params = EvaluateParams::new(expression);
    params.context_id = target.context_id;
    params.return_by_value = Some(false);
    params.await_promise = Some(true);
    params.user_gesture = Some(true);
    let result = page.evaluate_expression(params).await?;
    let object_id =
        result
            .object()
            .object_id
            .clone()
            .ok_or_else(|| BrowserError::Actionability {
                selector: target.query.display(),
                reason: "resolved element did not produce a remote object".to_owned(),
            })?;
    let quads = page
        .execute(
            GetContentQuadsParams::builder()
                .object_id(object_id.clone())
                .build(),
        )
        .await;
    let _ = page.execute(ReleaseObjectParams::new(object_id)).await;
    let quads = quads?;
    quads
        .quads
        .iter()
        .find_map(|quad| quad_center(quad.inner()))
        .ok_or_else(|| BrowserError::Actionability {
            selector: target.query.display(),
            reason: "element has no visible content quad".to_owned(),
        })
}

pub(super) async fn wait_until_actionable(
    page: &Page,
    target: &ElementTarget,
    requirement: Actionability,
) -> Result<(), BrowserError> {
    let deadline = tokio::time::Instant::now() + DEFAULT_WAIT_TIMEOUT;
    let expression = actionability_expression(&target.query, requirement)?;
    let mut last_reason = "element is not ready".to_owned();
    loop {
        let state: ActionabilityWire =
            evaluate_typed_in_context(page, &expression, target.context_id).await?;
        if state.count > 1 {
            return Err(BrowserError::StrictSelectorViolation {
                selector: target.query.display(),
                count: state.count,
            });
        }
        if state.state == "ready" {
            return Ok(());
        }
        if let Some(reason) = state.reason {
            last_reason = reason;
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(BrowserError::ActionabilityTimeout {
                selector: target.query.display(),
                reason: last_reason,
            });
        }
        sleep(POLL_INTERVAL).await;
    }
}

pub(super) async fn dispatch_click(
    page: &Page,
    point: Point,
    options: BrowserClickOptions,
) -> Result<(), BrowserError> {
    if !(1..=3).contains(&options.click_count) {
        return Err(BrowserError::InvalidClickCount {
            click_count: options.click_count,
        });
    }
    let button = cdp_mouse_button(options.button);
    let modifiers = modifier_mask(&options.modifiers);
    for click_count in 1..=options.click_count {
        let base = DispatchMouseEventParams::builder()
            .x(point.x)
            .y(point.y)
            .button(button.clone())
            .click_count(i64::from(click_count))
            .modifiers(modifiers);
        page.execute(
            base.clone()
                .r#type(DispatchMouseEventType::MousePressed)
                .build()
                .map_err(|message| BrowserError::Configuration { message })?,
        )
        .await?;
        page.execute(
            base.r#type(DispatchMouseEventType::MouseReleased)
                .build()
                .map_err(|message| BrowserError::Configuration { message })?,
        )
        .await?;
    }
    Ok(())
}

pub(super) async fn dispatch_mouse_move(
    page: &Page,
    from: Point,
    to: Point,
    steps: u16,
    buttons: i64,
) -> Result<(), BrowserError> {
    for step in 1..=steps {
        let progress = f64::from(step) / f64::from(steps);
        let mut event = DispatchMouseEventParams::new(
            DispatchMouseEventType::MouseMoved,
            from.x + (to.x - from.x) * progress,
            from.y + (to.y - from.y) * progress,
        );
        event.buttons = Some(buttons);
        page.execute(event).await?;
    }
    Ok(())
}

pub(super) async fn dispatch_mouse_button(
    page: &Page,
    point: Point,
    button: BrowserMouseButton,
    modifiers: &[BrowserKeyModifier],
    pressed: bool,
    buttons: i64,
) -> Result<(), BrowserError> {
    let mut event = DispatchMouseEventParams::new(
        if pressed {
            DispatchMouseEventType::MousePressed
        } else {
            DispatchMouseEventType::MouseReleased
        },
        point.x,
        point.y,
    );
    event.button = Some(cdp_mouse_button(button));
    event.buttons = Some(buttons);
    event.click_count = Some(1);
    event.modifiers = Some(modifier_mask(modifiers));
    page.execute(event).await?;
    Ok(())
}

pub(super) async fn dispatch_mouse_wheel(
    page: &Page,
    point: Point,
    delta_x: f64,
    delta_y: f64,
    buttons: i64,
) -> Result<(), BrowserError> {
    let mut event =
        DispatchMouseEventParams::new(DispatchMouseEventType::MouseWheel, point.x, point.y);
    event.delta_x = Some(delta_x);
    event.delta_y = Some(delta_y);
    event.buttons = Some(buttons);
    page.execute(event).await?;
    Ok(())
}

pub(super) async fn dispatch_touch_tap(page: &Page, point: Point) -> Result<(), BrowserError> {
    page.execute(TouchEventParams::new(
        DispatchTouchEventType::TouchStart,
        vec![TouchPoint::new(point.x, point.y)],
    ))
    .await?;
    page.execute(TouchEventParams::new(
        DispatchTouchEventType::TouchEnd,
        Vec::new(),
    ))
    .await?;
    Ok(())
}

pub(super) async fn dispatch_touch_swipe(
    page: &Page,
    from: Point,
    to: Point,
    duration: Duration,
    steps: u16,
) -> Result<(), BrowserError> {
    page.execute(TouchEventParams::new(
        DispatchTouchEventType::TouchStart,
        vec![TouchPoint::new(from.x, from.y)],
    ))
    .await?;
    let step_delay = duration / u32::from(steps);
    for step in 1..=steps {
        let progress = f64::from(step) / f64::from(steps);
        page.execute(TouchEventParams::new(
            DispatchTouchEventType::TouchMove,
            vec![TouchPoint::new(
                from.x + (to.x - from.x) * progress,
                from.y + (to.y - from.y) * progress,
            )],
        ))
        .await?;
        sleep(step_delay).await;
    }
    page.execute(TouchEventParams::new(
        DispatchTouchEventType::TouchEnd,
        Vec::new(),
    ))
    .await?;
    Ok(())
}

pub(super) async fn dispatch_keyboard_event(
    page: &Page,
    key: &str,
    modifiers: &[BrowserKeyModifier],
    pressed: bool,
) -> Result<(), BrowserError> {
    let definition = keys::get_key_definition(key).ok_or_else(|| BrowserError::UnknownKey {
        key: key.to_owned(),
    })?;
    let event_type = if pressed {
        if definition.text.is_some() || definition.key.len() == 1 {
            DispatchKeyEventType::KeyDown
        } else {
            DispatchKeyEventType::RawKeyDown
        }
    } else {
        DispatchKeyEventType::KeyUp
    };
    let mut event = DispatchKeyEventParams::new(event_type);
    if pressed {
        event.text = definition.text.map(str::to_owned);
    }
    event.key = Some(definition.key.to_owned());
    event.code = Some(definition.code.to_owned());
    event.windows_virtual_key_code = Some(definition.key_code);
    event.native_virtual_key_code = Some(definition.key_code);
    event.modifiers = Some(modifier_mask(modifiers));
    page.execute(event).await?;
    Ok(())
}

pub(super) async fn insert_text(page: &Page, text: String) -> Result<(), BrowserError> {
    page.execute(InsertTextParams::new(text)).await?;
    Ok(())
}

pub(super) const fn mouse_button_mask(button: BrowserMouseButton) -> i64 {
    match button {
        BrowserMouseButton::Left => 1,
        BrowserMouseButton::Right => 2,
        BrowserMouseButton::Middle => 4,
    }
}

const fn cdp_mouse_button(button: BrowserMouseButton) -> CdpMouseButton {
    match button {
        BrowserMouseButton::Left => CdpMouseButton::Left,
        BrowserMouseButton::Right => CdpMouseButton::Right,
        BrowserMouseButton::Middle => CdpMouseButton::Middle,
    }
}

pub(super) async fn dispatch_drag(
    page: &Page,
    source: Point,
    target: Point,
) -> Result<(), BrowserError> {
    let mut intercepted = page.event_listener::<EventDragIntercepted>().await?;
    page.execute(SetInterceptDragsParams::new(true)).await?;
    let drag_data = async {
        page.execute(DispatchMouseEventParams::new(
            DispatchMouseEventType::MouseMoved,
            source.x,
            source.y,
        ))
        .await?;
        let mut pressed =
            DispatchMouseEventParams::new(DispatchMouseEventType::MousePressed, source.x, source.y);
        pressed.button = Some(CdpMouseButton::Left);
        pressed.buttons = Some(1);
        pressed.click_count = Some(1);
        page.execute(pressed).await?;
        for step in 1..=3 {
            let progress = f64::from(step) / 20.0;
            let mut moved = DispatchMouseEventParams::new(
                DispatchMouseEventType::MouseMoved,
                source.x + (target.x - source.x) * progress,
                source.y + (target.y - source.y) * progress,
            );
            moved.button = Some(CdpMouseButton::Left);
            moved.buttons = Some(1);
            page.execute(moved).await?;
        }
        let intercepted = timeout(Duration::from_secs(2), intercepted.next())
            .await
            .ok()
            .flatten();
        if let Some(event) = &intercepted {
            trace_serialized("devtools.Input.dragIntercepted", event.as_ref());
        }
        Ok::<_, BrowserError>(intercepted.map_or_else(
            || DragData {
                items: vec![DragDataItem::new("text/plain", "")],
                files: None,
                drag_operations_mask: 1,
            },
            |event| event.data.clone(),
        ))
    }
    .await;
    let disable = page.execute(SetInterceptDragsParams::new(false)).await;
    let drag_data = match (drag_data, disable) {
        (Ok(data), Ok(_)) => data,
        (Err(error), _) => {
            let _ = release_mouse(page, target).await;
            return Err(error);
        }
        (Ok(_), Err(error)) => {
            let _ = release_mouse(page, target).await;
            return Err(error.into());
        }
    };
    let dispatched = async {
        page.execute(DispatchDragEventParams::new(
            DispatchDragEventType::DragEnter,
            target.x,
            target.y,
            drag_data.clone(),
        ))
        .await?;
        page.execute(DispatchDragEventParams::new(
            DispatchDragEventType::DragOver,
            target.x,
            target.y,
            drag_data.clone(),
        ))
        .await?;
        page.execute(DispatchDragEventParams::new(
            DispatchDragEventType::Drop,
            target.x,
            target.y,
            drag_data,
        ))
        .await?;
        Ok::<_, BrowserError>(())
    }
    .await;
    let released = release_mouse(page, target).await;
    dispatched?;
    released
}

async fn release_mouse(page: &Page, target: Point) -> Result<(), BrowserError> {
    let mut released =
        DispatchMouseEventParams::new(DispatchMouseEventType::MouseReleased, target.x, target.y);
    released.button = Some(CdpMouseButton::Left);
    released.click_count = Some(1);
    page.execute(released).await?;
    Ok(())
}

pub(super) fn modifier_mask(modifiers: &[BrowserKeyModifier]) -> i64 {
    modifiers.iter().fold(0, |mask, modifier| {
        mask | match modifier {
            BrowserKeyModifier::Alt => 1,
            BrowserKeyModifier::Control => 2,
            BrowserKeyModifier::Meta => 4,
            BrowserKeyModifier::Shift => 8,
        }
    })
}

pub(super) async fn wait_for_selector(
    page: &Page,
    target: &ElementTarget,
    state: BrowserWaitForSelectorState,
) -> Result<(), BrowserError> {
    let deadline = tokio::time::Instant::now() + DEFAULT_WAIT_TIMEOUT;
    let expression = selector_state_expression(&target.query)?;
    loop {
        let current: SelectorStateWire =
            evaluate_typed_in_context(page, &expression, target.context_id).await?;
        if current.count > 1 {
            return Err(BrowserError::StrictSelectorViolation {
                selector: target.query.display(),
                count: current.count,
            });
        }
        let ready = match state {
            BrowserWaitForSelectorState::Attached => current.attached,
            BrowserWaitForSelectorState::Visible => current.visible,
            BrowserWaitForSelectorState::Hidden => !current.visible,
            BrowserWaitForSelectorState::Detached => !current.attached,
        };
        if ready {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(BrowserError::SelectorStateTimeout {
                selector: target.query.display(),
                state,
            });
        }
        sleep(Duration::from_millis(50)).await;
    }
}

pub(super) async fn wait_for_text(
    page: &Page,
    target: Option<&ElementTarget>,
    text: &str,
    hidden: bool,
) -> Result<(), BrowserError> {
    let deadline = tokio::time::Instant::now() + DEFAULT_WAIT_TIMEOUT;
    let text = serde_json::to_string(text)?;
    let (expression, context_id) = if let Some(target) = target {
        (
            super::element_script(
                &target.query,
                &format!(
                    "return (element.innerText ?? element.textContent ?? \"\").includes({text});"
                ),
            )?,
            target.context_id,
        )
    } else {
        (
            format!(
                "Boolean((document.body?.innerText ?? document.body?.textContent ?? \"\").includes({text}))"
            ),
            None,
        )
    };
    loop {
        let present: bool = evaluate_typed_in_context(page, &expression, context_id).await?;
        if present != hidden {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(BrowserError::TextTimeout {
                text: serde_json::from_str(&text).unwrap_or(text),
                hidden,
            });
        }
        sleep(Duration::from_millis(50)).await;
    }
}

pub(super) async fn wait_for_url(page: &Page, contains: &str) -> Result<(), BrowserError> {
    let deadline = tokio::time::Instant::now() + DEFAULT_WAIT_TIMEOUT;
    loop {
        if page.url().await?.is_some_and(|url| url.contains(contains)) {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(BrowserError::UrlTimeout {
                expected: contains.to_owned(),
            });
        }
        sleep(Duration::from_millis(50)).await;
    }
}

pub(super) async fn wait_for_load_state(
    page: &Page,
    state: BrowserLoadState,
) -> Result<(), BrowserError> {
    let deadline = tokio::time::Instant::now() + DEFAULT_WAIT_TIMEOUT;
    loop {
        let ready: String = evaluate_typed(page, "document.readyState").await?;
        let reached = match state {
            BrowserLoadState::DomContentLoaded => {
                matches!(ready.as_str(), "interactive" | "complete")
            }
            BrowserLoadState::Load => ready == "complete",
        };
        if reached {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(BrowserError::LoadStateTimeout { state });
        }
        sleep(Duration::from_millis(50)).await;
    }
}

pub(super) async fn wait_for_function(page: &Page, expression: &str) -> Result<(), BrowserError> {
    let deadline = tokio::time::Instant::now() + DEFAULT_WAIT_TIMEOUT;
    let expression = format!("(async () => Boolean(await ({expression})))()");
    loop {
        if evaluate_typed::<bool>(page, &expression).await? {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(BrowserError::FunctionTimeout);
        }
        sleep(Duration::from_millis(50)).await;
    }
}

pub(super) struct ActionObservation {
    console_after: u64,
    errors_after: u64,
    requests_after: u64,
    download_ids: BTreeSet<String>,
}

impl ActionObservation {
    pub(super) fn capture(diagnostics: &Arc<StdMutex<Diagnostics>>) -> Result<Self, BrowserError> {
        let diagnostics = diagnostics
            .lock()
            .map_err(|_| BrowserError::DiagnosticsUnavailable)?;
        Ok(Self {
            console_after: diagnostics.next_console_sequence,
            errors_after: diagnostics.next_error_sequence,
            requests_after: diagnostics.next_request_sequence,
            download_ids: diagnostics.downloads.keys().cloned().collect(),
        })
    }

    pub(super) const fn console_after(&self) -> u64 {
        self.console_after
    }

    pub(super) const fn errors_after(&self) -> u64 {
        self.errors_after
    }

    pub(super) fn new_downloads(&self, diagnostics: &Diagnostics) -> Vec<crate::BrowserDownload> {
        diagnostics
            .downloads
            .iter()
            .filter(|(id, _)| !self.download_ids.contains(*id))
            .map(|(_, download)| download.clone())
            .collect()
    }
}

pub(super) async fn wait_for_completion(
    page: &Page,
    diagnostics: &Arc<StdMutex<Diagnostics>>,
    observation: &ActionObservation,
) -> Result<BrowserActionNetwork, BrowserError> {
    let started = Instant::now();
    sleep(ACTION_SETTLE).await;
    let deadline = tokio::time::Instant::now() + ACTION_NETWORK_TIMEOUT;
    let mut last_count = usize::MAX;
    let mut quiet_since = tokio::time::Instant::now();
    loop {
        let requests = {
            let diagnostics = diagnostics
                .lock()
                .map_err(|_| BrowserError::DiagnosticsUnavailable)?;
            action_requests(&diagnostics.requests, observation.requests_after)
        };
        if requests.len() != last_count {
            last_count = requests.len();
            quiet_since = tokio::time::Instant::now();
        }
        let relevant = requests
            .iter()
            .filter(|request| relevant_request(&request.request.resource_type))
            .collect::<Vec<_>>();
        let navigation = relevant.iter().any(|request| {
            request
                .request
                .resource_type
                .eq_ignore_ascii_case("document")
        });
        let pending = relevant.iter().any(|request| !request.request.completed);
        let document_ready = if navigation {
            evaluate_typed::<String>(page, "document.readyState")
                .await
                .is_ok_and(|state| state == "complete")
        } else {
            true
        };
        if !pending
            && document_ready
            && tokio::time::Instant::now().duration_since(quiet_since) >= ACTION_SETTLE
        {
            return Ok(network_summary(&requests, navigation, false, started));
        }
        if tokio::time::Instant::now() >= deadline {
            return Ok(network_summary(&requests, navigation, true, started));
        }
        sleep(POLL_INTERVAL).await;
    }
}

pub(super) const fn empty_network() -> BrowserActionNetwork {
    BrowserActionNetwork {
        request_count: 0,
        completed_count: 0,
        failed_count: 0,
        navigation: false,
        timed_out: false,
        waited_ms: 0,
    }
}

pub(super) fn snapshot_matches(
    snapshot: &str,
    refs: &BTreeMap<String, crate::BrowserElementReference>,
    query: &str,
    maximum: usize,
) -> (
    Vec<BrowserSnapshotMatch>,
    BTreeMap<String, crate::BrowserElementReference>,
    bool,
) {
    let lines = snapshot.lines().map(str::to_owned).collect::<Vec<_>>();
    let query_lower = query.to_lowercase();
    let matching = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| line.to_lowercase().contains(&query_lower).then_some(index))
        .collect::<Vec<_>>();
    let truncated = matching.len() > maximum;
    let mut matches = Vec::with_capacity(matching.len().min(maximum));
    for index in matching.into_iter().take(maximum) {
        let indent = leading_spaces(&lines[index]);
        let mut ancestors = Vec::new();
        let mut ancestor_indent = indent;
        for line in lines[..index].iter().rev() {
            let candidate_indent = leading_spaces(line);
            if candidate_indent < ancestor_indent {
                ancestors.push(line.clone());
                ancestor_indent = candidate_indent;
                if candidate_indent == 0 {
                    break;
                }
            }
        }
        ancestors.reverse();
        let before_from = index.saturating_sub(3);
        let after_to = (index + 4).min(lines.len());
        matches.push(BrowserSnapshotMatch {
            line_number: index + 1,
            text: lines[index].clone(),
            ancestors,
            before: lines[before_from..index].to_vec(),
            after: lines[index + 1..after_to].to_vec(),
        });
    }
    let visible = matches
        .iter()
        .flat_map(|item| {
            item.ancestors
                .iter()
                .chain(&item.before)
                .chain(std::iter::once(&item.text))
                .chain(&item.after)
        })
        .collect::<Vec<_>>();
    let matching_refs = refs
        .iter()
        .filter(|(reference, _)| {
            let marker = format!("[ref={reference}]");
            visible.iter().any(|line| line.contains(&marker))
        })
        .map(|(reference, metadata)| (reference.clone(), metadata.clone()))
        .collect();
    (matches, matching_refs, truncated)
}

fn network_summary(
    requests: &[NetworkEntry],
    navigation: bool,
    timed_out: bool,
    started: Instant,
) -> BrowserActionNetwork {
    BrowserActionNetwork {
        request_count: requests.len(),
        completed_count: requests
            .iter()
            .filter(|request| request.request.completed && request.request.failure.is_none())
            .count(),
        failed_count: requests
            .iter()
            .filter(|request| request.request.failure.is_some())
            .count(),
        navigation,
        timed_out,
        waited_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    }
}

fn action_requests(
    requests: &std::collections::VecDeque<NetworkEntry>,
    after: u64,
) -> Vec<NetworkEntry> {
    requests
        .iter()
        .filter(|entry| entry.sequence > after)
        .cloned()
        .collect()
}

fn relevant_request(resource_type: &str) -> bool {
    matches!(
        resource_type.to_ascii_lowercase().as_str(),
        "document" | "stylesheet" | "script" | "xhr" | "fetch"
    )
}

fn quad_center(quad: &[f64]) -> Option<Point> {
    if quad.len() != 8 {
        return None;
    }
    let area = (0..4)
        .map(|index| {
            let next = (index + 1) % 4;
            quad[index * 2] * quad[next * 2 + 1] - quad[next * 2] * quad[index * 2 + 1]
        })
        .sum::<f64>()
        .abs()
        / 2.0;
    (area > 1.0).then(|| {
        Point::new(
            (quad[0] + quad[2] + quad[4] + quad[6]) / 4.0,
            (quad[1] + quad[3] + quad[5] + quad[7]) / 4.0,
        )
    })
}

fn actionability_expression(
    query: &ElementQuery,
    requirement: Actionability,
) -> Result<String, serde_json::Error> {
    let query = query_resolver_prelude(query)?;
    let requirement = serde_json::to_string(requirement.as_str())?;
    Ok(format!(
        r#"(async () => {{
{query}
const requirement = {requirement};
if (matches.length !== 1) return {{
  state: matches.length ? "strict" : "missing",
  reason: matches.length ? "selector matched multiple elements" : "selector did not match an element",
  count: matches.length
}};
const element = matches[0];
if (!element.isConnected) return {{ state: "waiting", reason: "element is detached", count: 1 }};
if (requirement === "attached") return {{ state: "ready", reason: null, count: 1 }};
const style = getComputedStyle(element);
let bounds = element.getBoundingClientRect();
const visible = style.display !== "none" &&
  style.visibility !== "hidden" &&
  style.visibility !== "collapse" &&
  bounds.width > 0 && bounds.height > 0;
if (!visible) return {{ state: "waiting", reason: "element is not visible", count: 1 }};
element.scrollIntoView({{ block: "center", inline: "center", behavior: "instant" }});
const frame = () => new Promise(resolve => requestAnimationFrame(resolve));
await frame();
const first = element.getBoundingClientRect();
await frame();
bounds = element.getBoundingClientRect();
const stable = Math.abs(first.x - bounds.x) <= 0.25 &&
  Math.abs(first.y - bounds.y) <= 0.25 &&
  Math.abs(first.width - bounds.width) <= 0.25 &&
  Math.abs(first.height - bounds.height) <= 0.25;
if (!stable) return {{ state: "waiting", reason: "element is moving", count: 1 }};
const disabled = element.matches?.(":disabled") ||
  element.getAttribute?.("aria-disabled") === "true";
if (["enabled", "editable", "pointer"].includes(requirement) && disabled) {{
  return {{ state: "waiting", reason: "element is disabled", count: 1 }};
}}
if (requirement === "editable") {{
  const editable = element instanceof HTMLInputElement ||
    element instanceof HTMLTextAreaElement ||
    element.isContentEditable;
  if (!editable) return {{ state: "waiting", reason: "element is not editable", count: 1 }};
  if (element.readOnly) return {{ state: "waiting", reason: "element is read-only", count: 1 }};
}}
if (requirement === "pointer") {{
  if (style.pointerEvents === "none") {{
    return {{ state: "waiting", reason: "element does not receive pointer events", count: 1 }};
  }}
  const x = bounds.left + bounds.width / 2;
  const y = bounds.top + bounds.height / 2;
  const root = element.getRootNode();
  const hit = root.elementFromPoint?.(x, y) ?? document.elementFromPoint(x, y);
  if (!hit || !(hit === element || element.contains(hit))) {{
    const blocker = hit
      ? `${{hit.tagName.toLowerCase()}}${{hit.id ? `#${{hit.id}}` : ""}}`
      : "nothing";
    return {{
      state: "waiting",
      reason: `element is obscured by ${{blocker}}`,
      count: 1
    }};
  }}
}}
return {{ state: "ready", reason: null, count: 1 }};
}})()"#,
    ))
}

fn element_expression(query: &ElementQuery) -> Result<String, serde_json::Error> {
    let query = query_resolver_prelude(query)?;
    Ok(format!(
        r"(() => {{
{query}
return matches[0] ?? null;
}})()"
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SelectorStateWire {
    attached: bool,
    visible: bool,
    count: usize,
}

fn selector_state_expression(query: &ElementQuery) -> Result<String, serde_json::Error> {
    let query = query_resolver_prelude(query)?;
    Ok(format!(
        r#"(() => {{
{query}
const elements = matches;
const visible = elements.some(element => {{
  const style = getComputedStyle(element);
  const bounds = element.getBoundingClientRect();
  return element.isConnected &&
    style.display !== "none" &&
    style.visibility !== "hidden" &&
    style.visibility !== "collapse" &&
    bounds.width > 0 && bounds.height > 0;
}});
return {{
  attached: elements.some(element => element.isConnected),
  visible,
  count: elements.length
}};
}})()"#
    ))
}

fn leading_spaces(line: &str) -> usize {
    line.len().saturating_sub(line.trim_start().len())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chromiumoxide::cdp::browser_protocol::input::DispatchTouchEventType;

    use crate::BrowserElementReference;

    use super::{TouchEventParams, snapshot_matches};

    #[test]
    fn touch_end_serializes_required_empty_touch_points() {
        let event = TouchEventParams::new(DispatchTouchEventType::TouchEnd, Vec::new());

        assert_eq!(
            serde_json::to_value(event).expect("touch event serializes"),
            serde_json::json!({"type": "touchEnd", "touchPoints": []})
        );
    }

    #[test]
    fn snapshot_search_returns_context_and_only_visible_references() {
        let snapshot = r#"main "Dashboard"
  heading "Deployments"
  generic "Production"
    button "Retry failed deployment" [ref=e1]
    button "Delete deployment" [ref=e2]
  heading "Billing"
    link "Invoices" [ref=e3]"#;
        let refs = [
            ("e1", "Retry failed deployment"),
            ("e2", "Delete deployment"),
            ("e3", "Invoices"),
        ]
        .into_iter()
        .map(|(reference, name)| {
            (
                reference.to_owned(),
                BrowserElementReference {
                    role: "button".to_owned(),
                    name: name.to_owned(),
                    disabled: false,
                    frame_url: None,
                    frame_id: None,
                },
            )
        })
        .collect::<BTreeMap<_, _>>();

        let (matches, matching_refs, truncated) = snapshot_matches(snapshot, &refs, "failed", 10);
        assert_eq!(matches.len(), 1);
        assert_eq!(
            matches[0].ancestors,
            ["main \"Dashboard\"", "  generic \"Production\""]
        );
        assert_eq!(
            matches[0].text,
            "    button \"Retry failed deployment\" [ref=e1]"
        );
        assert!(!truncated);
        assert!(matching_refs.contains_key("e1"));
        assert!(matching_refs.contains_key("e2"));
        assert!(matching_refs.contains_key("e3"));
    }
}
