//! Actual Windows UI Automation client. All COM objects are created, used and
//! released on one MTA worker; the Desktop receives only owned Rust data.
use super::windows_uia_model::{
    self as model, Operation, Provider, RequestContext, Root, RuntimeIdentity, Worker,
};
use crate::{Error, Result, ax::Node, selection::TextRange};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};
use windows::{
    Win32::{
        Foundation::HWND,
        System::{Com::*, Ole::*, Variant::*},
        UI::Accessibility::*,
    },
    core::{BSTR, Interface},
};

const MAX_NODES: usize = 5000;
const MAX_TEXT: usize = 65536;
fn com<T>(result: windows::core::Result<T>) -> Result<T> {
    result.map_err(|error| {
        Error::action(format!(
            "UI Automation: {} (HRESULT 0x{:08x})",
            error.message(),
            error.code().0 as u32
        ))
    })
}
pub fn start() -> Result<Worker> {
    Worker::spawn(
        || Ok(Box::new(ComProvider::new()?)),
        Duration::from_secs(10),
    )
}
struct Apartment;
impl Apartment {
    fn enter() -> Result<Self> {
        com(unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok() })?;
        Ok(Self)
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}
#[derive(Clone)]
struct Saved {
    identity: RuntimeIdentity,
    node: Node,
    element: IUIAutomationElement,
}
struct WindowCache {
    runtime: Vec<i32>,
    elements: BTreeMap<String, Saved>,
}
struct CaptureContext<'a> {
    root: Root,
    root_runtime: &'a [i32],
    elements: &'a mut BTreeMap<String, Saved>,
    seen: BTreeSet<Vec<i32>>,
    request: &'a RequestContext,
}
struct ComProvider {
    windows: BTreeMap<Root, WindowCache>,
    automation: IUIAutomation,
    cache: IUIAutomationCacheRequest,
    single: IUIAutomationCacheRequest,
    walker: IUIAutomationTreeWalker,
    // Struct fields drop in declaration order; CoUninitialize must come last.
    _apartment: Apartment,
}
impl ComProvider {
    fn new() -> Result<Self> {
        let apartment = Apartment::enter()?;
        let automation: IUIAutomation =
            com(unsafe { CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER) })?;
        let settings: IUIAutomation2 = com(automation.cast())?;
        com(unsafe { settings.SetConnectionTimeout(2000) })?;
        com(unsafe { settings.SetTransactionTimeout(2000) })?;
        let single = cache(&automation, false)?;
        let cache = cache(&automation, true)?;
        let walker = com(unsafe { automation.RawViewWalker() })?;
        Ok(Self {
            windows: BTreeMap::new(),
            automation,
            cache,
            single,
            walker,
            _apartment: apartment,
        })
    }
    fn root(&self, root: Root) -> Result<IUIAutomationElement> {
        validate_window(root)?;
        let element = com(unsafe {
            self.automation
                .ElementFromHandleBuildCache(HWND(root.hwnd as _), &self.cache)
        })?;
        if com(unsafe { element.CachedProcessId() })? != root.pid as i32 {
            return Err(Error::action(
                "UIA root process differs from the approved window",
            ));
        }
        Ok(element)
    }
    fn collect(
        &self,
        element: IUIAutomationElement,
        depth: usize,
        capture: &mut CaptureContext<'_>,
    ) -> Result<Node> {
        capture.request.check()?;
        if depth > 100 || capture.elements.len() >= MAX_NODES {
            return Err(Error::action("UIA tree exceeds capture bounds"));
        }
        let runtime = runtime_id(&element)?;
        if !capture.seen.insert(runtime.clone()) {
            return Err(Error::action(
                "UIA tree contains a repeated runtime identity",
            ));
        }
        let identity = RuntimeIdentity {
            root: capture.root,
            root_runtime: capture.root_runtime.to_vec(),
            process: com(unsafe { element.CachedProcessId() })?,
            runtime,
        };
        identity.validate()?;
        let mut node = properties(&element, identity.key(), depth == 0, capture.request)?;
        let child_list = children(&element)?;
        if child_list.len() > MAX_NODES.saturating_sub(capture.elements.len() + 1) {
            return Err(Error::action("UIA child array exceeds capture bounds"));
        }
        capture.elements.insert(
            node.identity.clone(),
            Saved {
                identity,
                node: node.clone(),
                element: element.clone(),
            },
        );
        for child in child_list {
            capture.request.check()?;
            let child = com(unsafe { child.BuildUpdatedCache(&self.cache) })?;
            node.children.push(self.collect(child, depth + 1, capture)?);
        }
        Ok(node)
    }
    fn resolve(
        &self,
        root: Root,
        identity: &str,
        operation: &Operation,
        ctx: &RequestContext,
    ) -> Result<Saved> {
        ctx.check()?;
        let cache = self
            .windows
            .get(&root)
            .ok_or_else(|| Error::action("Request window text before using UIA elements"))?;
        let saved = cache
            .elements
            .get(identity)
            .ok_or_else(|| Error::action("UIA element is not in the latest window state"))?;
        let root_element = self.root(root)?;
        if runtime_id(&root_element)? != cache.runtime {
            return Err(Error::action("UIA window runtime identity changed"));
        }
        let element = com(unsafe { saved.element.BuildUpdatedCache(&self.single) })?;
        let current_identity = RuntimeIdentity {
            root,
            root_runtime: cache.runtime.clone(),
            process: com(unsafe { element.CachedProcessId() })?,
            runtime: runtime_id(&element)?,
        };
        let node = properties(
            &element,
            identity.into(),
            saved.identity.runtime == cache.runtime,
            ctx,
        )?;
        model::revalidate(
            &saved.identity,
            &current_identity,
            &saved.node,
            &node,
            operation,
        )?;
        // Reparenting an existing element does not necessarily change its runtime ID.
        // Walk fresh raw-view parents until the exact current approved root is found.
        let mut ancestor = element.clone();
        let mut visited = BTreeSet::new();
        for _ in 0..=100 {
            ctx.check()?;
            if com(unsafe { self.automation.CompareElements(&ancestor, &root_element) })?.as_bool()
            {
                validate_window(root)?;
                return Ok(Saved {
                    identity: current_identity,
                    node,
                    element,
                });
            }
            if !visited.insert(runtime_id(&ancestor)?) {
                return Err(Error::action("UIA ancestry contains a cycle"));
            }
            ancestor = com(unsafe { self.walker.GetParentElement(&ancestor) })?;
        }
        Err(Error::action(
            "UIA target is no longer inside the approved window",
        ))
    }
}
impl Provider for ComProvider {
    fn observe(&mut self, root: Root, ctx: &RequestContext) -> Result<Node> {
        ctx.check()?;
        let element = self.root(root)?;
        let runtime = runtime_id(&element)?;
        let mut elements = BTreeMap::new();
        let tree = self.collect(
            element,
            0,
            &mut CaptureContext {
                root,
                root_runtime: &runtime,
                elements: &mut elements,
                seen: BTreeSet::new(),
                request: ctx,
            },
        )?;
        ctx.check()?;
        validate_window(root)?;
        if runtime_id(&self.root(root)?)? != runtime {
            return Err(Error::action("UIA root changed during capture"));
        }
        if self.windows.len() >= 128 && !self.windows.contains_key(&root) {
            return Err(Error::action("UIA window cache limit exceeded"));
        }
        self.windows.insert(root, WindowCache { runtime, elements });
        Ok(tree)
    }
    fn perform(
        &mut self,
        root: Root,
        identity: &str,
        operation: Operation,
        ctx: &RequestContext,
    ) -> Result<Option<[f64; 2]>> {
        let saved = self.resolve(root, identity, &operation, ctx)?;
        let element = &saved.element;
        ctx.check()?;
        match operation {
            Operation::Point => {
                if com(unsafe { element.CurrentIsOffscreen() })?.as_bool() {
                    return Err(Error::action("UIA target is offscreen"));
                }
                let mut point = windows::Win32::Foundation::POINT::default();
                if !com(unsafe { element.GetClickablePoint(&mut point) })?.as_bool() {
                    return Err(Error::action("UIA provider reported no clickable point"));
                }
                ctx.check()?;
                validate_window(root)?;
                return Ok(Some([point.x as f64, point.y as f64]));
            }
            Operation::Invoke => {
                let p: IUIAutomationInvokePattern = pattern(element, UIA_InvokePatternId)?;
                ctx.check()?;
                com(unsafe { p.Invoke() })?;
            }
            Operation::Toggle => {
                let p: IUIAutomationTogglePattern = pattern(element, UIA_TogglePatternId)?;
                ctx.check()?;
                com(unsafe { p.Toggle() })?;
            }
            Operation::Expand | Operation::Collapse => {
                let p: IUIAutomationExpandCollapsePattern =
                    pattern(element, UIA_ExpandCollapsePatternId)?;
                if com(unsafe { p.CurrentExpandCollapseState() })? == ExpandCollapseState_LeafNode {
                    return Err(Error::unsupported("UIA leaf cannot expand or collapse"));
                }
                ctx.check()?;
                if matches!(operation, Operation::Expand) {
                    com(unsafe { p.Expand() })?;
                } else {
                    com(unsafe { p.Collapse() })?;
                }
            }
            Operation::Select | Operation::AddSelection | Operation::RemoveSelection => {
                let p: IUIAutomationSelectionItemPattern =
                    pattern(element, UIA_SelectionItemPatternId)?;
                ctx.check()?;
                match operation {
                    Operation::Select => com(unsafe { p.Select() })?,
                    Operation::AddSelection => com(unsafe { p.AddToSelection() })?,
                    _ => com(unsafe { p.RemoveFromSelection() })?,
                }
            }
            Operation::ScrollIntoView => {
                let p: IUIAutomationScrollItemPattern = pattern(element, UIA_ScrollItemPatternId)?;
                ctx.check()?;
                com(unsafe { p.ScrollIntoView() })?;
            }
            Operation::Scroll {
                horizontal,
                vertical,
            } => {
                let p: IUIAutomationScrollPattern = pattern(element, UIA_ScrollPatternId)?;
                let x = scroll_amount(horizontal)?;
                let y = scroll_amount(vertical)?;
                if horizontal != 0 && !com(unsafe { p.CurrentHorizontallyScrollable() })?.as_bool()
                {
                    return Err(Error::unsupported("UIA target cannot scroll horizontally"));
                }
                if vertical != 0 && !com(unsafe { p.CurrentVerticallyScrollable() })?.as_bool() {
                    return Err(Error::unsupported("UIA target cannot scroll vertically"));
                }
                ctx.check()?;
                com(unsafe { p.Scroll(x, y) })?;
            }
            Operation::Focus => {
                ctx.check()?;
                com(unsafe { element.SetFocus() })?;
            }
            Operation::SetValue(value) => {
                if value.encode_utf16().count() > MAX_TEXT {
                    return Err(Error::invalid("UIA value exceeds text limit"));
                }
                if available(element, UIA_IsValuePatternAvailablePropertyId)? {
                    let p: IUIAutomationValuePattern = pattern(element, UIA_ValuePatternId)?;
                    if com(unsafe { p.CurrentIsReadOnly() })?.as_bool() {
                        return Err(Error::action("UIA value is read-only"));
                    }
                    let value = BSTR::from(value);
                    ctx.check()?;
                    com(unsafe { p.SetValue(&value) })?;
                } else {
                    let p: IUIAutomationRangeValuePattern =
                        pattern(element, UIA_RangeValuePatternId)?;
                    let value = model::range_value(
                        &value,
                        com(unsafe { p.CurrentMinimum() })?,
                        com(unsafe { p.CurrentMaximum() })?,
                        com(unsafe { p.CurrentIsReadOnly() })?.as_bool(),
                    )?;
                    ctx.check()?;
                    com(unsafe { p.SetValue(value) })?;
                }
            }
            Operation::SelectText(range) => {
                if com(unsafe { element.CurrentIsPassword() })?.as_bool() {
                    return Err(Error::unsupported("UIA password text is not exposed"));
                }
                select_text(element, range, ctx)?;
            }
        }
        ctx.check()?;
        validate_window(root)?;
        Ok(None)
    }
    fn clear(&mut self) -> Result<()> {
        self.windows.clear();
        Ok(())
    }
}
fn validate_window(root: Root) -> Result<()> {
    root.validate()?;
    let hwnd = root.hwnd as windows_sys::Win32::Foundation::HWND;
    let mut pid = 0;
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(hwnd, &mut pid);
    }
    if unsafe { windows_sys::Win32::UI::WindowsAndMessaging::IsWindow(hwnd) } == 0
        || pid != root.pid
    {
        return Err(Error::action("Approved HWND/PID identity changed"));
    }
    Ok(())
}
fn pattern<T: Interface>(element: &IUIAutomationElement, id: UIA_PATTERN_ID) -> Result<T> {
    com(unsafe { element.GetCurrentPatternAs(id) })
}
fn available(element: &IUIAutomationElement, id: UIA_PROPERTY_ID) -> Result<bool> {
    let value = com(unsafe { element.GetCachedPropertyValue(id) })?;
    if value.vt() != VT_BOOL {
        return Err(Error::action(
            "UIA pattern-availability property has invalid type",
        ));
    }
    com(bool::try_from(&value))
}
fn cache(automation: &IUIAutomation, children: bool) -> Result<IUIAutomationCacheRequest> {
    let cache = com(unsafe { automation.CreateCacheRequest() })?;
    com(unsafe {
        cache.SetTreeScope(if children {
            TreeScope(TreeScope_Element.0 | TreeScope_Children.0)
        } else {
            TreeScope_Element
        })
    })?;
    com(unsafe { cache.SetTreeFilter(&com(automation.RawViewCondition())?) })?;
    com(unsafe { cache.SetAutomationElementMode(AutomationElementMode_Full) })?;
    for property in [
        UIA_ProcessIdPropertyId,
        UIA_ControlTypePropertyId,
        UIA_LocalizedControlTypePropertyId,
        UIA_NamePropertyId,
        UIA_HelpTextPropertyId,
        UIA_AutomationIdPropertyId,
        UIA_ClassNamePropertyId,
        UIA_IsEnabledPropertyId,
        UIA_HasKeyboardFocusPropertyId,
        UIA_IsKeyboardFocusablePropertyId,
        UIA_IsOffscreenPropertyId,
        UIA_IsPasswordPropertyId,
        UIA_BoundingRectanglePropertyId,
        UIA_IsInvokePatternAvailablePropertyId,
        UIA_IsValuePatternAvailablePropertyId,
        UIA_IsRangeValuePatternAvailablePropertyId,
        UIA_IsTogglePatternAvailablePropertyId,
        UIA_IsExpandCollapsePatternAvailablePropertyId,
        UIA_IsSelectionItemPatternAvailablePropertyId,
        UIA_IsScrollItemPatternAvailablePropertyId,
        UIA_IsScrollPatternAvailablePropertyId,
        UIA_IsTextPatternAvailablePropertyId,
        UIA_ValueValuePropertyId,
        UIA_ValueIsReadOnlyPropertyId,
        UIA_RangeValueValuePropertyId,
        UIA_RangeValueIsReadOnlyPropertyId,
        UIA_RangeValueMinimumPropertyId,
        UIA_RangeValueMaximumPropertyId,
        UIA_ToggleToggleStatePropertyId,
        UIA_ExpandCollapseExpandCollapseStatePropertyId,
        UIA_SelectionItemIsSelectedPropertyId,
        UIA_ScrollHorizontallyScrollablePropertyId,
        UIA_ScrollVerticallyScrollablePropertyId,
    ] {
        com(unsafe { cache.AddProperty(property) })?;
    }
    for pattern in [
        UIA_InvokePatternId,
        UIA_ValuePatternId,
        UIA_RangeValuePatternId,
        UIA_TogglePatternId,
        UIA_ExpandCollapsePatternId,
        UIA_SelectionItemPatternId,
        UIA_ScrollPatternId,
        UIA_ScrollItemPatternId,
        UIA_TextPatternId,
    ] {
        com(unsafe { cache.AddPattern(pattern) })?;
    }
    Ok(cache)
}
fn runtime_id(element: &IUIAutomationElement) -> Result<Vec<i32>> {
    let array = com(unsafe { element.GetRuntimeId() })?;
    if array.is_null() {
        return Err(Error::action("UIA runtime ID array is null"));
    }
    let read = (|| -> Result<Vec<i32>> {
        if unsafe { SafeArrayGetDim(array) } != 1
            || com(unsafe { SafeArrayGetVartype(array) })? != VT_I4
            || unsafe { SafeArrayGetElemsize(array) } != 4
        {
            return Err(Error::action("UIA runtime ID is not an Int32 vector"));
        }
        let lower = com(unsafe { SafeArrayGetLBound(array, 1) })?;
        let upper = com(unsafe { SafeArrayGetUBound(array, 1) })?;
        if upper < lower || i64::from(upper) - i64::from(lower) >= 128 {
            return Err(Error::action("UIA runtime ID length is invalid"));
        }
        let mut out = Vec::new();
        for index in lower..=upper {
            let mut value = 0i32;
            com(unsafe { SafeArrayGetElement(array, &index, (&mut value as *mut i32).cast()) })?;
            out.push(value);
        }
        Ok(out)
    })();
    let cleanup = com(unsafe { SafeArrayDestroy(array) });
    let values = read?;
    cleanup?;
    Ok(values)
}
fn children(element: &IUIAutomationElement) -> Result<Vec<IUIAutomationElement>> {
    // S_OK + null is the documented empty-child result, distinct from failure.
    let mut raw = std::ptr::null_mut();
    com(unsafe { (element.vtable().GetCachedChildren)(element.as_raw(), &mut raw).ok() })?;
    if raw.is_null() {
        return Ok(Vec::new());
    }
    let array = unsafe { IUIAutomationElementArray::from_raw(raw) };
    let len = com(unsafe { array.Length() })?;
    if !(0..=MAX_NODES as i32).contains(&len) {
        return Err(Error::action("UIA child count is invalid"));
    }
    (0..len)
        .map(|index| com(unsafe { array.GetElement(index) }))
        .collect()
}
fn bounded(value: BSTR) -> Result<String> {
    if value.len() > MAX_TEXT {
        return Err(Error::action("UIA string exceeds capture limit"));
    }
    Ok(value.to_string())
}
fn optional(value: String) -> Option<String> {
    if value.is_empty() { None } else { Some(value) }
}
#[allow(non_upper_case_globals)] // Preserve the Windows SDK constant names.
fn properties(
    element: &IUIAutomationElement,
    identity: String,
    root: bool,
    ctx: &RequestContext,
) -> Result<Node> {
    ctx.check()?;
    let control = com(unsafe { element.CachedControlType() })?;
    let password = com(unsafe { element.CachedIsPassword() })?.as_bool();
    let rect = com(unsafe { element.CachedBoundingRectangle() })?;
    let mut node = Node {
        identity,
        role: if root { "AXWindow" } else { role(control) }.into(),
        title: optional(bounded(com(unsafe { element.CachedName() })?)?),
        identifier: optional(bounded(com(unsafe { element.CachedAutomationId() })?)?),
        subrole: optional(bounded(com(unsafe { element.CachedClassName() })?)?),
        role_description: optional(bounded(com(unsafe {
            element.CachedLocalizedControlType()
        })?)?),
        help: optional(bounded(com(unsafe { element.CachedHelpText() })?)?),
        enabled: com(unsafe { element.CachedIsEnabled() })?.as_bool(),
        focused: com(unsafe { element.CachedHasKeyboardFocus() })?.as_bool(),
        focusable: com(unsafe { element.CachedIsKeyboardFocusable() })?.as_bool(),
        frame: Some([
            rect.left as f64,
            rect.top as f64,
            (i64::from(rect.right) - i64::from(rect.left)) as f64,
            (i64::from(rect.bottom) - i64::from(rect.top)) as f64,
        ]),
        ..Default::default()
    };
    let mut details = Vec::new();
    if com(unsafe { element.CachedIsOffscreen() })?.as_bool() {
        details.push("offscreen".into());
    }
    if password {
        details.push("password".into());
    }
    for (availability, action) in [
        (UIA_IsInvokePatternAvailablePropertyId, "Invoke"),
        (UIA_IsTogglePatternAvailablePropertyId, "Toggle"),
        (UIA_IsScrollItemPatternAvailablePropertyId, "ScrollIntoView"),
    ] {
        if available(element, availability)? {
            node.actions.push(action.into());
        }
    }
    if available(element, UIA_IsValuePatternAvailablePropertyId)? {
        let p: IUIAutomationValuePattern =
            com(unsafe { element.GetCachedPatternAs(UIA_ValuePatternId) })?;
        node.settable = !com(unsafe { p.CachedIsReadOnly() })?.as_bool();
        if !password {
            node.value = Some(bounded(com(unsafe { p.CachedValue() })?)?);
        }
    } else if available(element, UIA_IsRangeValuePatternAvailablePropertyId)? {
        let p: IUIAutomationRangeValuePattern =
            com(unsafe { element.GetCachedPatternAs(UIA_RangeValuePatternId) })?;
        node.settable = !com(unsafe { p.CachedIsReadOnly() })?.as_bool();
        node.numeric_value = true;
        let value = com(unsafe { p.CachedValue() })?;
        if !value.is_finite() {
            return Err(Error::action("UIA range value is non-finite"));
        }
        if !password {
            node.value = Some(value.to_string());
        }
    }
    if available(element, UIA_IsTogglePatternAvailablePropertyId)? {
        let p: IUIAutomationTogglePattern =
            com(unsafe { element.GetCachedPatternAs(UIA_TogglePatternId) })?;
        let state = com(unsafe { p.CachedToggleState() })?;
        details.push(format!("toggle={}", state.0));
        node.value = Some(
            match state {
                ToggleState_On => "1",
                ToggleState_Off => "0",
                _ => "mixed",
            }
            .into(),
        );
    }
    if available(element, UIA_IsExpandCollapsePatternAvailablePropertyId)? {
        let p: IUIAutomationExpandCollapsePattern =
            com(unsafe { element.GetCachedPatternAs(UIA_ExpandCollapsePatternId) })?;
        let state = com(unsafe { p.CachedExpandCollapseState() })?;
        details.push(format!("expandCollapse={}", state.0));
        if state != ExpandCollapseState_LeafNode {
            node.actions.extend(["Expand".into(), "Collapse".into()]);
        }
    }
    if available(element, UIA_IsSelectionItemPatternAvailablePropertyId)? {
        let p: IUIAutomationSelectionItemPattern =
            com(unsafe { element.GetCachedPatternAs(UIA_SelectionItemPatternId) })?;
        node.selected = com(unsafe { p.CachedIsSelected() })?.as_bool();
        node.selectable = true;
        node.actions.extend([
            "Select".into(),
            "AddToSelection".into(),
            "RemoveFromSelection".into(),
        ]);
    }
    if available(element, UIA_IsScrollPatternAvailablePropertyId)? {
        let p: IUIAutomationScrollPattern =
            com(unsafe { element.GetCachedPatternAs(UIA_ScrollPatternId) })?;
        if com(unsafe { p.CachedHorizontallyScrollable() })?.as_bool() {
            node.actions
                .extend(["ScrollLeft".into(), "ScrollRight".into()]);
        }
        if com(unsafe { p.CachedVerticallyScrollable() })?.as_bool() {
            node.actions
                .extend(["ScrollUp".into(), "ScrollDown".into()]);
        }
    }
    if !password && available(element, UIA_IsTextPatternAvailablePropertyId)? {
        ctx.check()?;
        let p: IUIAutomationTextPattern =
            com(unsafe { element.GetCachedPatternAs(UIA_TextPatternId) })?;
        if node.value.is_none() {
            node.value = Some(bounded(com(unsafe {
                com(p.DocumentRange())?.GetText(MAX_TEXT as i32 + 1)
            })?)?);
        }
        if com(unsafe { p.SupportedTextSelection() })? != SupportedTextSelection_None {
            let ranges = com(unsafe { p.GetSelection() })?;
            let count = com(unsafe { ranges.Length() })?;
            if !(0..=64).contains(&count) {
                return Err(Error::action("UIA text selection count exceeds bounds"));
            }
            let mut selected = Vec::new();
            for index in 0..count {
                ctx.check()?;
                let range = com(unsafe { ranges.GetElement(index) })?;
                selected.push(bounded(com(unsafe {
                    range.GetText(MAX_TEXT as i32 + 1)
                })?)?);
            }
            node.selected_text = Some(selected.join("\n"));
        }
    }
    node.detail = optional(details.join(", "));
    Ok(node)
}
#[allow(non_upper_case_globals)] // Preserve the Windows SDK constant names.
fn role(control: UIA_CONTROLTYPE_ID) -> &'static str {
    match control {
        UIA_ButtonControlTypeId => "AXButton",
        UIA_CheckBoxControlTypeId => "AXCheckBox",
        UIA_ComboBoxControlTypeId => "AXComboBox",
        UIA_EditControlTypeId => "AXTextField",
        UIA_DocumentControlTypeId => "AXTextArea",
        UIA_HyperlinkControlTypeId => "AXLink",
        UIA_ImageControlTypeId => "AXImage",
        UIA_ListItemControlTypeId => "AXRow",
        UIA_ListControlTypeId => "AXList",
        UIA_MenuControlTypeId => "AXMenu",
        UIA_MenuBarControlTypeId => "AXMenuBar",
        UIA_MenuItemControlTypeId => "AXMenuItem",
        UIA_ProgressBarControlTypeId => "AXProgressIndicator",
        UIA_RadioButtonControlTypeId => "AXRadioButton",
        UIA_ScrollBarControlTypeId => "AXScrollBar",
        UIA_SliderControlTypeId => "AXSlider",
        UIA_SpinnerControlTypeId => "AXIncrementor",
        UIA_TabControlTypeId => "AXTabGroup",
        UIA_TabItemControlTypeId => "AXRadioButton",
        UIA_TextControlTypeId => "AXStaticText",
        UIA_ToolBarControlTypeId => "AXToolbar",
        UIA_TreeControlTypeId => "AXOutline",
        UIA_TreeItemControlTypeId => "AXRow",
        UIA_DataGridControlTypeId | UIA_TableControlTypeId => "AXTable",
        UIA_DataItemControlTypeId => "AXCell",
        UIA_WindowControlTypeId => "AXWindow",
        UIA_HeaderControlTypeId => "AXGroup",
        UIA_HeaderItemControlTypeId => "AXColumn",
        _ => "AXGroup",
    }
}
fn scroll_amount(value: i32) -> Result<ScrollAmount> {
    match value {
        -2 => Ok(ScrollAmount_LargeDecrement),
        -1 => Ok(ScrollAmount_SmallDecrement),
        0 => Ok(ScrollAmount_NoAmount),
        1 => Ok(ScrollAmount_SmallIncrement),
        2 => Ok(ScrollAmount_LargeIncrement),
        _ => Err(Error::invalid("Invalid UIA scroll amount")),
    }
}
fn select_text(
    element: &IUIAutomationElement,
    range: TextRange,
    ctx: &RequestContext,
) -> Result<()> {
    let pattern: IUIAutomationTextPattern = pattern(element, UIA_TextPatternId)?;
    if com(unsafe { pattern.SupportedTextSelection() })? == SupportedTextSelection_None {
        return Err(Error::unsupported(
            "UIA target does not support text selection",
        ));
    }
    let document = com(unsafe { pattern.DocumentRange() })?;
    let text = bounded(com(unsafe { document.GetText(MAX_TEXT as i32 + 1) })?)?;
    let units: Vec<u16> = text.encode_utf16().collect();
    let end = range
        .location
        .checked_add(range.length)
        .filter(|end| *end <= units.len())
        .ok_or_else(|| Error::invalid("UIA text range is out of bounds"))?;
    let expected = String::from_utf16(&units[range.location..end])
        .map_err(|_| Error::invalid("UIA text range splits a surrogate"))?;
    let start = endpoint(&document, &units, range.location, ctx)?;
    let finish = endpoint(&document, &units, end, ctx)?;
    com(unsafe {
        start.MoveEndpointByRange(
            TextPatternRangeEndpoint_End,
            &finish,
            TextPatternRangeEndpoint_Start,
        )
    })?;
    if bounded(com(unsafe { start.GetText(MAX_TEXT as i32 + 1) })?)? != expected {
        return Err(Error::action(
            "UIA source changed while preparing selection",
        ));
    }
    ctx.check()?;
    com(unsafe { start.Select() })?;
    let actual = com(unsafe { pattern.GetSelection() })?;
    if com(unsafe { actual.Length() })? != 1 {
        return Err(Error::action(
            "UIA selection did not produce one exact range",
        ));
    }
    let actual = com(unsafe { actual.GetElement(0) })?;
    for which in [TextPatternRangeEndpoint_Start, TextPatternRangeEndpoint_End] {
        if com(unsafe { actual.CompareEndpoints(which, &start, which) })? != 0 {
            return Err(Error::action(
                "UIA selected endpoints did not match the requested range",
            ));
        }
    }
    Ok(())
}
/// UIA Character units are provider-defined, not presumed to be UTF-16 units.
/// Binary-search the unit endpoint by the actual UTF-16 prefix it exposes.
fn endpoint(
    document: &IUIAutomationTextRange,
    source: &[u16],
    offset: usize,
    ctx: &RequestContext,
) -> Result<IUIAutomationTextRange> {
    let probe = model::locate_text_endpoint(source, offset, |count| {
        ctx.check()?;
        let probe = com(unsafe { document.Clone() })?;
        com(unsafe {
            probe.MoveEndpointByRange(
                TextPatternRangeEndpoint_End,
                &probe,
                TextPatternRangeEndpoint_Start,
            )
        })?;
        let moved = com(unsafe {
            probe.MoveEndpointByUnit(
                TextPatternRangeEndpoint_End,
                TextUnit_Character,
                count as i32,
            )
        })?;
        let moved = usize::try_from(moved)
            .map_err(|_| Error::action("UIA text provider returned negative movement"))?;
        let prefix = bounded(com(unsafe { probe.GetText(MAX_TEXT as i32 + 1) })?)?;
        Ok((moved, prefix.encode_utf16().collect(), probe))
    })?;
    com(unsafe {
        probe.MoveEndpointByRange(
            TextPatternRangeEndpoint_Start,
            &probe,
            TextPatternRangeEndpoint_End,
        )
    })?;
    Ok(probe)
}
