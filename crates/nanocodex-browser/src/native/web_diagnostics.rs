use chromiumoxide::{
    Page, cdp::browser_protocol::page::AddScriptToEvaluateOnNewDocumentParams,
    cdp::js_protocol::runtime::ExecutionContextId,
};
use serde::Deserialize;

use crate::{
    BrowserAccessibilityAudit, BrowserAccessibilityImpact, BrowserAccessibilityViolation,
    BrowserWebVitals,
};

use super::{BrowserError, evaluate_typed_in_context};

const INSTALL_SCRIPT: &str = r#"(() => {
  if (globalThis.__nanocodexWebDiagnostics) return;
  const state = {
    largestContentfulPaint: null,
    cumulativeLayoutShift: 0,
    interactionToNextPaint: null,
    longTasks: []
  };
  Object.defineProperty(globalThis, "__nanocodexWebDiagnostics", {
    value: state,
    configurable: false,
    enumerable: false,
    writable: false
  });
  const observe = (type, callback) => {
    try {
      const observer = new PerformanceObserver((list) => callback(list.getEntries()));
      observer.observe({ type, buffered: true });
    } catch (_) {}
  };
  observe("largest-contentful-paint", (entries) => {
    for (const entry of entries) {
      state.largestContentfulPaint = Math.max(
        state.largestContentfulPaint || 0,
        Number(entry.startTime) || 0
      );
    }
  });
  observe("layout-shift", (entries) => {
    for (const entry of entries) {
      if (!entry.hadRecentInput) {
        state.cumulativeLayoutShift += Number(entry.value) || 0;
      }
    }
  });
  observe("event", (entries) => {
    for (const entry of entries) {
      if (!entry.interactionId) continue;
      state.interactionToNextPaint = Math.max(
        state.interactionToNextPaint || 0,
        Number(entry.duration) || 0
      );
    }
  });
  observe("longtask", (entries) => {
    for (const entry of entries) {
      state.longTasks.push(Number(entry.duration) || 0);
      if (state.longTasks.length > 4096) state.longTasks.shift();
    }
  });
})();"#;

const READ_WEB_VITALS: &str = r#"(() => {
  const state = globalThis.__nanocodexWebDiagnostics || {
    largestContentfulPaint: null,
    cumulativeLayoutShift: 0,
    interactionToNextPaint: null,
    longTasks: []
  };
  const navigation = performance.getEntriesByType("navigation")[0] || null;
  const paints = performance.getEntriesByType("paint");
  const firstContentfulPaint = paints.find((entry) =>
    entry.name === "first-contentful-paint"
  );
  const resources = performance.getEntriesByType("resource");
  const transferredBytes = resources.reduce(
    (sum, entry) => sum + Math.max(0, Number(entry.transferSize) || 0),
    0
  );
  const totalBlockingTime = state.longTasks.reduce(
    (sum, duration) => sum + Math.max(0, duration - 50),
    0
  );
  return {
    url: location.href,
    firstContentfulPaintMs: firstContentfulPaint
      ? Number(firstContentfulPaint.startTime)
      : null,
    largestContentfulPaintMs: state.largestContentfulPaint,
    cumulativeLayoutShift: Number(state.cumulativeLayoutShift) || 0,
    interactionToNextPaintMs: state.interactionToNextPaint,
    timeToFirstByteMs: navigation ? Number(navigation.responseStart) : null,
    domContentLoadedMs: navigation
      ? Number(navigation.domContentLoadedEventEnd)
      : null,
    loadMs: navigation ? Number(navigation.loadEventEnd) : null,
    longTaskCount: state.longTasks.length,
    totalBlockingTimeMs: totalBlockingTime,
    resourceCount: resources.length,
    transferredBytes
  };
})()"#;

const READ_LAYOUT_SHIFT: &str = r"(() =>
  Number(globalThis.__nanocodexWebDiagnostics?.cumulativeLayoutShift) || 0
)()";

const ACCESSIBILITY_AUDIT: &str = r#"(() => {
  const violations = [];
  const all = [];
  const visit = (root) => {
    for (const element of root.querySelectorAll("*")) {
      all.push(element);
      if (element.shadowRoot) visit(element.shadowRoot);
    }
  };
  visit(document);
  const selector = (element) => {
    if (element.id) return `#${CSS.escape(element.id)}`;
    const parts = [];
    let current = element;
    while (current && current.nodeType === Node.ELEMENT_NODE && parts.length < 5) {
      let part = current.localName;
      if (!part) break;
      const parent = current.parentElement;
      if (parent) {
        const siblings = Array.from(parent.children).filter(
          (candidate) => candidate.localName === current.localName
        );
        if (siblings.length > 1) part += `:nth-of-type(${siblings.indexOf(current) + 1})`;
      }
      parts.unshift(part);
      const root = current.getRootNode();
      if (root instanceof ShadowRoot) {
        current = root.host;
        parts.unshift("::shadow");
      } else {
        current = parent;
      }
    }
    return parts.join(" > ");
  };
  const push = (rule, impact, message, element) => violations.push({
    rule,
    impact,
    message,
    selector: selector(element)
  });
  for (const element of all) {
    if (element instanceof HTMLImageElement &&
        !element.hasAttribute("alt") &&
        element.getAttribute("role") !== "presentation") {
      push("image-alt", "critical", "Image has no alt text", element);
    }
    if (element.matches("button, [role='button'], a[href]")) {
      const name = (
        element.getAttribute("aria-label") ||
        element.getAttribute("aria-labelledby") ||
        element.textContent ||
        ""
      ).trim();
      if (!name) push("interactive-name", "serious", "Interactive element has no accessible name", element);
    }
    if (element.matches("input:not([type='hidden']), textarea, select")) {
      const id = element.id;
      const labelled = Boolean(
        element.getAttribute("aria-label") ||
        element.getAttribute("aria-labelledby") ||
        (id && document.querySelector(`label[for="${CSS.escape(id)}"]`)) ||
        element.closest("label")
      );
      if (!labelled) push("form-label", "critical", "Form control has no label", element);
    }
    if (element.hasAttribute("tabindex") &&
        Number(element.getAttribute("tabindex")) > 0) {
      push("positive-tabindex", "moderate", "Positive tabindex changes natural keyboard order", element);
    }
    if (element.getAttribute("aria-hidden") === "true" &&
        element.matches("button, a[href], input, select, textarea, [tabindex]")) {
      push("aria-hidden-focus", "serious", "Focusable element is hidden from assistive technology", element);
    }
  }
  if (!document.documentElement.lang) {
    push("html-lang", "serious", "Document language is missing", document.documentElement);
  }
  if (!document.title.trim()) {
    push("document-title", "serious", "Document title is missing", document.documentElement);
  }
  const ids = new Map();
  for (const element of all) {
    if (!element.id) continue;
    if (ids.has(element.id)) {
      push("duplicate-id", "moderate", `Duplicate id "${element.id}"`, element);
    } else {
      ids.set(element.id, element);
    }
  }
  let previousHeading = 0;
  for (const heading of all.filter((element) => /^H[1-6]$/.test(element.tagName))) {
    const level = Number(heading.tagName.slice(1));
    if (previousHeading && level > previousHeading + 1) {
      push("heading-order", "moderate", `Heading level jumps from h${previousHeading} to h${level}`, heading);
    }
    previousHeading = level;
  }
  return {
    url: location.href,
    checkedElements: all.length,
    violations
  };
})()"#;

pub(super) async fn install(page: &Page) -> Result<(), BrowserError> {
    let mut params = AddScriptToEvaluateOnNewDocumentParams::new(INSTALL_SCRIPT);
    params.run_immediately = Some(true);
    page.add_init_script(params).await?;
    Ok(())
}

pub(super) async fn web_vitals(page: &Page) -> Result<BrowserWebVitals, BrowserError> {
    evaluate_typed_in_context(page, READ_WEB_VITALS, None).await
}

pub(super) async fn cumulative_layout_shift(page: &Page) -> Result<f64, BrowserError> {
    evaluate_typed_in_context(page, READ_LAYOUT_SHIFT, None).await
}

pub(super) async fn accessibility_audit(
    page: &Page,
    context_id: Option<ExecutionContextId>,
    frame_id: Option<String>,
    frame_url: Option<String>,
) -> Result<BrowserAccessibilityAudit, BrowserError> {
    let wire: AccessibilityAuditWire =
        evaluate_typed_in_context(page, ACCESSIBILITY_AUDIT, context_id).await?;
    Ok(BrowserAccessibilityAudit {
        url: wire.url,
        checked_elements: wire.checked_elements,
        violations: wire
            .violations
            .into_iter()
            .map(|violation| BrowserAccessibilityViolation {
                rule: violation.rule,
                impact: violation.impact.into(),
                message: violation.message,
                selector: violation.selector,
                frame_id: frame_id.clone(),
                frame_url: frame_url.clone(),
            })
            .collect(),
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AccessibilityAuditWire {
    url: String,
    checked_elements: usize,
    violations: Vec<AccessibilityViolationWire>,
}

#[derive(Deserialize)]
struct AccessibilityViolationWire {
    rule: String,
    impact: AccessibilityImpactWire,
    message: String,
    selector: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum AccessibilityImpactWire {
    Critical,
    Serious,
    Moderate,
    Minor,
}

impl From<AccessibilityImpactWire> for BrowserAccessibilityImpact {
    fn from(value: AccessibilityImpactWire) -> Self {
        match value {
            AccessibilityImpactWire::Critical => Self::Critical,
            AccessibilityImpactWire::Serious => Self::Serious,
            AccessibilityImpactWire::Moderate => Self::Moderate,
            AccessibilityImpactWire::Minor => Self::Minor,
        }
    }
}
