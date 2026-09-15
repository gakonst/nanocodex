//! OS operations are confined to this provider. No OpenAI executable, signing
//! identity, service socket or captured implementation is loaded by the rebuild.
use super::{Action, App, Desktop, Image, Target};
#[path = "apps.rs"]
pub mod apps;
#[path = "monitor.rs"]
mod monitor;
#[path = "references.rs"]
mod references;
use crate::{
    Error, Result,
    ax::Node,
    clipboard::{self, Item, Pasteboard},
};
use accessibility_sys::*;
use block2::RcBlock;
use core_foundation::{
    array::CFArray,
    attributed_string::{
        CFAttributedString, CFAttributedStringGetAttributes, CFAttributedStringGetString,
    },
    base::{CFEqual, CFType, TCFType},
    boolean::CFBoolean,
    dictionary::CFDictionary,
    number::CFNumber,
    runloop::{CFRunLoop, kCFRunLoopDefaultMode},
    string::CFString,
    url::CFURL,
};
use core_graphics::{
    event::{CGEvent, CGEventFlags, CGEventTapLocation, CGEventType, CGMouseButton, EventField},
    event_source::{CGEventSource, CGEventSourceStateID},
    geometry::{CGPoint, CGSize},
};
use monitor::Monitor;
use objc2::{
    AnyThread, DefinedClass, define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, ProtocolObject},
};
use objc2_app_kit::{
    NSAccessibilityAttachmentTextAttribute, NSAccessibilityLinkTextAttribute,
    NSAccessibilityListItemIndexTextAttribute, NSAccessibilityListItemLevelTextAttribute,
    NSAccessibilityListItemPrefixTextAttribute, NSApplicationActivationOptions, NSFont,
    NSFontDescriptor, NSFontFamilyAttribute, NSFontNameAttribute, NSPasteboard, NSPasteboardItem,
    NSPasteboardItemDataProvider, NSPasteboardType, NSPasteboardWriting, NSRunningApplication,
    NSWorkspace,
};
use objc2_foundation::{
    NSArray, NSData, NSDictionary, NSError, NSNumber, NSObject, NSObjectProtocol, NSString, NSURL,
};
use objc2_screen_capture_kit::{
    SCContentFilter, SCScreenshotManager, SCShareableContent, SCStreamConfiguration,
};
use std::{
    collections::BTreeMap,
    ffi::c_void,
    ptr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Clone)]
struct Ax(CFType);

/// Preserve unavailable, failed, and malformed AX reads separately. Callers that
/// need optional layout can discard the error; screenshot authority must not.
fn checked_ax_frame(mut read: impl FnMut(&str) -> Result<Option<CFType>>) -> Result<[f64; 4]> {
    let mut attribute = |name: &str| -> Result<CFType> {
        let value = read(name)
            .map_err(|error| {
                Error::new(error.code, format!("Cannot read {name}: {}", error.message))
            })?
            .ok_or_else(|| {
                Error::action(format!("Window geometry attribute {name} is unavailable"))
            })?;
        if value.type_of() != unsafe { AXValueGetTypeID() } {
            return Err(Error::action(format!(
                "Window geometry attribute {name} is not an AXValue"
            )));
        }
        Ok(value)
    };
    let position = attribute("AXPosition")?;
    let size = attribute("AXSize")?;
    let mut point = CGPoint::new(0., 0.);
    let mut extent = CGSize::new(0., 0.);
    if !unsafe {
        AXValueGetValue(
            position.as_CFTypeRef() as _,
            kAXValueTypeCGPoint,
            &mut point as *mut _ as *mut c_void,
        )
    } {
        return Err(Error::action(
            "Window AXPosition does not contain a CGPoint",
        ));
    }
    if !unsafe {
        AXValueGetValue(
            size.as_CFTypeRef() as _,
            kAXValueTypeCGSize,
            &mut extent as *mut _ as *mut c_void,
        )
    } {
        return Err(Error::action("Window AXSize does not contain a CGSize"));
    }
    Ok([point.x, point.y, extent.width, extent.height])
}

fn context_frame(
    require_geometry: bool,
    optional: impl FnOnce() -> Option<[f64; 4]>,
    checked: impl FnOnce() -> Result<[f64; 4]>,
) -> Result<Option<[f64; 4]>> {
    if require_geometry {
        checked().map(Some)
    } else {
        Ok(optional())
    }
}

/// A failed read is not proof of movement. Every uncertain or changed context
/// still rejects capture before its geometry can be published.
fn check_screenshot_context(
    same_window: bool,
    before_frame: [f64; 4],
    after_frame: impl FnOnce() -> Result<[f64; 4]>,
    before_displays: &super::screenshot::Displays,
    after_displays: impl FnOnce() -> Result<super::screenshot::Displays>,
) -> Result<()> {
    if !same_window {
        return Err(Error::action(
            "Window identity changed during screenshot capture; query get_app_state again",
        ));
    }
    let after_frame = after_frame().map_err(|error| {
        Error::new(
            error.code,
            format!(
                "Cannot verify window geometry after screenshot capture: {}",
                error.message
            ),
        )
    })?;
    if before_frame != after_frame {
        return Err(Error::action(format!(
            "Window frame changed during screenshot capture ({before_frame:?} -> {after_frame:?}); query get_app_state again"
        )));
    }
    let after_displays = after_displays().map_err(|error| {
        Error::new(
            error.code,
            format!(
                "Cannot verify display geometry after screenshot capture: {}",
                error.message
            ),
        )
    })?;
    if *before_displays != after_displays {
        return Err(Error::action(format!(
            "Display geometry changed during screenshot capture ({before_displays:?} -> {after_displays:?}); query get_app_state again"
        )));
    }
    Ok(())
}
struct AttributedEntry {
    source: String,
    range: Option<crate::selection::TextRange>,
    input: Option<super::render::AttributedInput>,
}
impl AttributedEntry {
    fn input_for(
        &self,
        source: &str,
        range: Option<crate::selection::TextRange>,
    ) -> Result<Option<&super::render::AttributedInput>> {
        if self.source != source || self.range != range {
            return Err(Error::action(
                "Attributed source or native range changed between observation views",
            ));
        }
        Ok(self.input.as_ref())
    }
}
struct TextContext {
    references: references::ReferenceCache,
    attributed: BTreeMap<String, AttributedEntry>,
    retained_bytes: usize,
    pid: i32,
    deadline: Instant,
    warnings: Vec<String>,
}

struct NativeTextReader<'a> {
    ax: &'a Ax,
    pid: i32,
    deadline: Instant,
    warnings: Vec<String>,
}
impl<'a> NativeTextReader<'a> {
    fn new(ax: &'a Ax, pid: i32, deadline: Instant) -> Result<Self> {
        use super::text_source::Provider;
        let mut reader = Self {
            ax,
            pid,
            deadline,
            warnings: Vec::new(),
        };
        reader.check()?;
        ax_ok(unsafe { AXUIElementSetMessagingTimeout(ax.ptr(), 0.25) })?;
        reader.check()?;
        Ok(reader)
    }
    fn warning(&mut self, name: &str, detail: &str) {
        if self.warnings.len() < 16 {
            self.warnings.push(format!("Optional {name}: {detail}"));
        }
    }
    fn get_optional(&mut self, name: &str) -> Result<Option<CFType>> {
        use super::text_source::Provider;
        self.check()?;
        let result = self.ax.get(name);
        self.check()?;
        match result {
            Ok(value) => Ok(value),
            Err(error) => {
                self.warning(name, &error.message);
                Ok(None)
            }
        }
    }
    fn decode_string(
        &mut self,
        value: Option<CFType>,
        name: &str,
        limit: usize,
    ) -> Result<Option<String>> {
        let Some(value) = value else { return Ok(None) };
        let Some(value) = value.downcast::<CFString>() else {
            self.warning(name, "value is not a native string");
            return Ok(None);
        };
        if value.char_len() < 0 || value.char_len() as usize > limit {
            return Err(Error::action(format!("{name} exceeds native text bound")));
        }
        Ok(Some(value.to_string()))
    }
}
impl super::text_source::Provider for NativeTextReader<'_> {
    fn check(&mut self) -> Result<()> {
        if Instant::now() >= self.deadline {
            return Err(Error::action("Native text source deadline exceeded"));
        }
        let mut pid = 0;
        ax_ok(unsafe { AXUIElementGetPid(self.ax.ptr(), &mut pid) })?;
        if pid <= 0 || pid != self.pid {
            return Err(Error::action(
                "Native text source belongs to another application",
            ));
        }
        if Instant::now() >= self.deadline {
            return Err(Error::action("Native text source deadline exceeded"));
        }
        Ok(())
    }
    fn visible_range(&mut self) -> Result<Option<crate::selection::TextRange>> {
        let Some(value) = self.get_optional("AXVisibleCharacterRange")? else {
            return Ok(None);
        };
        let mut range = core_foundation::base::CFRange {
            location: 0,
            length: 0,
        };
        if value.type_of() != unsafe { AXValueGetTypeID() }
            || !unsafe {
                AXValueGetValue(
                    value.as_CFTypeRef() as _,
                    kAXValueTypeCFRange,
                    &mut range as *mut _ as _,
                )
            }
        {
            self.warning("AXVisibleCharacterRange", "value is not a native range");
            return Ok(None);
        }
        // Zero/nonpositive length follows the recovered default-range branch.
        if range.length <= 0 {
            return Ok(None);
        }
        if range.location < 0 {
            return Err(Error::action("AXVisibleCharacterRange has invalid origin"));
        }
        let range = crate::selection::TextRange {
            location: range.location as usize,
            length: range.length as usize,
        };
        super::text_source::validate_range(range)?;
        Ok(Some(range))
    }
    fn string_for_range(
        &mut self,
        requested: crate::selection::TextRange,
    ) -> Result<Option<String>> {
        super::text_source::validate_range(requested)?;
        self.check()?;
        let range = core_foundation::base::CFRange {
            location: requested.location as isize,
            length: requested.length as isize,
        };
        let raw = unsafe { AXValueCreate(kAXValueTypeCFRange, &range as *const _ as _) };
        if raw.is_null() {
            return Err(Error::action("Cannot allocate native string range"));
        }
        let range = unsafe { CFType::wrap_under_create_rule(raw as _) };
        let mut result = ptr::null();
        let code = unsafe {
            AXUIElementCopyParameterizedAttributeValue(
                self.ax.ptr(),
                CFString::new("AXStringForRange").as_concrete_TypeRef(),
                range.as_CFTypeRef(),
                &mut result,
            )
        };
        let value = if result.is_null() {
            None
        } else {
            Some(unsafe { CFType::wrap_under_create_rule(result) })
        };
        self.check()?;
        if code != 0 {
            if code != kAXErrorParameterizedAttributeUnsupported
                && code != kAXErrorAttributeUnsupported
                && code != kAXErrorNoValue
            {
                self.warning(
                    "AXStringForRange",
                    &format!("Accessibility operation failed ({code})"),
                );
            }
            return Ok(None);
        }
        self.decode_string(
            value,
            "AXStringForRange",
            super::text_source::MAX_RANGE_UNITS,
        )
    }
    fn refresh_value(&mut self) -> Result<Option<String>> {
        let value = self.get_optional("AXValue")?;
        self.decode_string(value, "AXValue", super::text_source::MAX_SOURCE_UNITS)
    }
    fn textual_context(&mut self) -> Result<Option<String>> {
        let value = self.get_optional("AXTextualContext")?;
        self.decode_string(value, "AXTextualContext", 16 * 1024)
    }
}
#[link(name = "CoreText", kind = "framework")]
unsafe extern "C" {
    fn CTFontGetSymbolicTraits(font: *const c_void) -> u32;
}
/// The recovered renderer reconstructs the font from its name/family and size,
/// then reads CoreText's actual traits. Typeface names are not style predicates.
fn font_traits(font: &CFDictionary<CFString, CFType>) -> u32 {
    let string = |key: &str| {
        font.find(CFString::new(key))
            .and_then(|v| v.downcast::<CFString>())
            .map(|v| v.to_string())
    };
    let Some(name) = string("AXFontName") else {
        return 0;
    };
    let Some(size) = font
        .find(CFString::new("AXFontSize"))
        .and_then(|v| v.downcast::<CFNumber>())
        .and_then(|v| v.to_f64())
        .filter(|n| n.is_finite() && *n >= 0.)
    else {
        return 0;
    };
    let name = NSString::from_str(&name);
    let family = string("AXFontFamily").map(|v| NSString::from_str(&v));
    let mut keys = vec![unsafe { NSFontNameAttribute }];
    let mut values: Vec<&AnyObject> = vec![name.as_ref()];
    if let Some(family) = &family {
        keys.push(unsafe { NSFontFamilyAttribute });
        values.push(family.as_ref());
    }
    let attributes = NSDictionary::from_slices(&keys, &values);
    let descriptor =
        unsafe { NSFontDescriptor::fontDescriptorWithFontAttributes(Some(&attributes)) };
    NSFont::fontWithDescriptor_size(&descriptor, size)
        .map(|font| unsafe { CTFontGetSymbolicTraits(Retained::as_ptr(&font).cast()) })
        .unwrap_or(0)
}
impl Ax {
    fn ptr(&self) -> AXUIElementRef {
        self.0.as_CFTypeRef() as AXUIElementRef
    }
    fn get(&self, name: &str) -> Result<Option<CFType>> {
        let name = CFString::new(name);
        let mut value = ptr::null();
        let code = unsafe {
            AXUIElementCopyAttributeValue(self.ptr(), name.as_concrete_TypeRef(), &mut value)
        };
        let value = if value.is_null() {
            None
        } else {
            Some(unsafe { CFType::wrap_under_create_rule(value) })
        };
        if code == kAXErrorNoValue || code == kAXErrorAttributeUnsupported {
            return Ok(None);
        }
        ax_ok(code)?;
        Ok(value)
    }
    fn text(&self, name: &str) -> Option<String> {
        let value = self.get(name).ok()??;
        if let Some(s) = value.downcast::<CFString>() {
            Some(s.to_string())
        } else if let Some(url) = value.downcast::<CFURL>() {
            Some(url.get_string().to_string())
        } else {
            value
                .downcast::<CFNumber>()
                .and_then(|n| n.to_f64())
                .map(|n| n.to_string())
        }
    }
    fn boolean(&self, name: &str) -> Option<bool> {
        self.get(name)
            .ok()??
            .downcast::<CFBoolean>()
            .map(|b| b.into())
    }
    fn element_checked(&self, name: &str) -> Result<Option<Ax>> {
        let Some(value) = self.get(name)? else {
            return Ok(None);
        };
        if value.type_of() != unsafe { AXUIElementGetTypeID() } {
            return Err(Error::action(format!("{name} is not an AX element")));
        }
        Ok(Some(Ax(value)))
    }
    fn elements_checked(&self, name: &str) -> Result<Vec<Ax>> {
        let Some(value) = self.get(name)? else {
            return Ok(vec![]);
        };
        Self::element_array(value, name)
    }
    fn title_relations(&self) -> Result<Vec<Ax>> {
        let name = "AXServesAsTitleForUIElements";
        let Some(value) = self.get(name)? else {
            return Ok(vec![]);
        };
        // A live AppKit fixture returns one AXUIElement here, despite the
        // plural attribute name. Normalize only this observed relationship;
        // structural children/windows remain strictly checked arrays.
        if value.type_of() == unsafe { AXUIElementGetTypeID() } {
            return Ok(vec![Ax(value)]);
        }
        Self::element_array(value, name)
    }
    fn selected_range(&self) -> Result<Option<crate::selection::TextRange>> {
        let Some(value) = self.get("AXSelectedTextRange")? else {
            return Ok(None);
        };
        let mut range = core_foundation::base::CFRange {
            location: 0,
            length: 0,
        };
        if value.type_of() != unsafe { AXValueGetTypeID() }
            || !unsafe {
                AXValueGetValue(
                    value.as_CFTypeRef() as _,
                    kAXValueTypeCFRange,
                    &mut range as *mut _ as _,
                )
            }
            || range.location < 0
            || range.length < 0
        {
            return Err(Error::action(
                "Focused text selection has invalid native range",
            ));
        }
        Ok(Some(crate::selection::TextRange {
            location: range.location as usize,
            length: range.length as usize,
        }))
    }
    fn element_array(value: CFType, name: &str) -> Result<Vec<Ax>> {
        let values = value.downcast::<CFArray>().ok_or_else(|| {
            let raw = unsafe { core_foundation::base::CFCopyTypeIDDescription(value.type_of()) };
            let actual = if raw.is_null() {
                "unknown".to_owned()
            } else {
                unsafe { CFString::wrap_under_create_rule(raw) }.to_string()
            };
            Error::action(format!(
                "{name} is not an AX array (actual CF type {actual}, id {})",
                value.type_of()
            ))
        })?;
        if values.len() > 5000 {
            return Err(Error::action(format!(
                "{name} exceeds AX collection bounds"
            )));
        }
        values
            .get_all_values()
            .into_iter()
            .map(|p| {
                if p.is_null() {
                    return Err(Error::action(format!("{name} has a null element")));
                }
                let value = unsafe { CFType::wrap_under_get_rule(p) };
                if value.type_of() != unsafe { AXUIElementGetTypeID() } {
                    return Err(Error::action(format!("{name} has a non-AX value")));
                }
                Ok(Ax(value))
            })
            .collect()
    }
    fn children(&self) -> Result<Vec<Ax>> {
        let name = CFString::new("AXChildren");
        let mut count = 0;
        let code = unsafe {
            AXUIElementGetAttributeValueCount(self.ptr(), name.as_concrete_TypeRef(), &mut count)
        };
        if code == kAXErrorAttributeUnsupported || code == kAXErrorNoValue {
            return Ok(vec![]);
        }
        ax_ok(code)?;
        if !(0..=5000).contains(&count) {
            return Err(Error::action("AX child count exceeds collection bounds"));
        }
        let mut result = Vec::with_capacity(count as usize);
        for start in (0..count).step_by(100) {
            let mut raw = ptr::null();
            let take = 100.min(count - start);
            ax_ok(unsafe {
                AXUIElementCopyAttributeValues(
                    self.ptr(),
                    name.as_concrete_TypeRef(),
                    start,
                    take,
                    &mut raw,
                )
            })?;
            if raw.is_null() {
                return Err(Error::action("AX child batch returned no array"));
            }
            let value = unsafe { CFType::wrap_under_create_rule(raw as _) };
            let values = value
                .downcast::<CFArray<*const c_void>>()
                .ok_or_else(|| Error::action("AX child batch is not an array"))?;
            if values.len() != take {
                return Err(Error::action("AX children changed during collection"));
            }
            for p in values.get_all_values() {
                if p.is_null() {
                    return Err(Error::action("Null AX child"));
                }
                let value = unsafe { CFType::wrap_under_get_rule(p) };
                if value.type_of() != unsafe { AXUIElementGetTypeID() } {
                    return Err(Error::action("Invalid AX child type"));
                }
                result.push(Ax(value));
            }
        }
        Ok(result)
    }
    fn set(&self, name: &str, value: &CFType) -> Result<()> {
        ax_ok(unsafe {
            AXUIElementSetAttributeValue(
                self.ptr(),
                CFString::new(name).as_concrete_TypeRef(),
                value.as_CFTypeRef(),
            )
        })
    }
    fn settable(&self, name: &str) -> bool {
        let mut result = 0;
        unsafe {
            AXUIElementIsAttributeSettable(
                self.ptr(),
                CFString::new(name).as_concrete_TypeRef(),
                &mut result,
            ) == 0
                && result != 0
        }
    }
    fn perform(&self, name: &str) -> Result<()> {
        ax_ok(unsafe {
            AXUIElementPerformAction(self.ptr(), CFString::new(name).as_concrete_TypeRef())
        })
    }
    fn frame(&self) -> Option<[f64; 4]> {
        let pos = self.get("AXPosition").ok()??;
        let size = self.get("AXSize").ok()??;
        if pos.type_of() != unsafe { AXValueGetTypeID() }
            || size.type_of() != unsafe { AXValueGetTypeID() }
        {
            return None;
        }
        let mut p = CGPoint::new(0., 0.);
        let mut s = CGSize::new(0., 0.);
        if unsafe {
            AXValueGetValue(
                pos.as_CFTypeRef() as _,
                kAXValueTypeCGPoint,
                &mut p as *mut _ as *mut c_void,
            ) && AXValueGetValue(
                size.as_CFTypeRef() as _,
                kAXValueTypeCGSize,
                &mut s as *mut _ as *mut c_void,
            )
        } {
            Some([p.x, p.y, s.width, s.height])
        } else {
            None
        }
    }
    fn checked_frame(&self) -> Result<[f64; 4]> {
        checked_ax_frame(|name| self.get(name))
    }
    fn attributed(
        &self,
        source: &str,
        requested: Option<crate::selection::TextRange>,
        text_context: &mut TextContext,
    ) -> Result<Option<super::render::AttributedInput>> {
        let length = source.encode_utf16().count();
        if length > 1024 * 1024 {
            return Err(Error::action("Attributed source exceeds length bound"));
        }
        if length == 0 {
            return Ok(None);
        }
        let requested = super::text_source::attributed_range(source, requested)?;
        let range = core_foundation::base::CFRange {
            location: requested.location as isize,
            length: requested.length as isize,
        };
        let raw = unsafe { AXValueCreate(kAXValueTypeCFRange, &range as *const _ as _) };
        if raw.is_null() {
            return Err(Error::action("Cannot allocate attributed text range"));
        }
        let range = unsafe { CFType::wrap_under_create_rule(raw as _) };
        let mut result = ptr::null();
        let code = unsafe {
            AXUIElementCopyParameterizedAttributeValue(
                self.ptr(),
                CFString::new("AXAttributedStringForRange").as_concrete_TypeRef(),
                range.as_CFTypeRef(),
                &mut result,
            )
        };
        if code == kAXErrorParameterizedAttributeUnsupported
            || code == kAXErrorAttributeUnsupported
            || code == kAXErrorNoValue
        {
            return Ok(None);
        }
        ax_ok(code)?;
        if result.is_null() {
            return Ok(None);
        }
        let value = unsafe { CFType::wrap_under_create_rule(result) };
        let Some(attributed) = value.downcast::<CFAttributedString>() else {
            return Err(Error::action("AX attributed text has invalid type"));
        };
        if attributed.char_len() != length as isize {
            return Ok(None);
        }
        let raw_string = unsafe { CFAttributedStringGetString(attributed.as_concrete_TypeRef()) };
        if raw_string.is_null() {
            return Err(Error::action("AX attributed text has no source"));
        }
        let text = unsafe { CFString::wrap_under_get_rule(raw_string) }.to_string();
        if text != source {
            return Ok(None);
        } // Do not apply stale source offsets.
        let list_level_key = unsafe { NSAccessibilityListItemLevelTextAttribute }.to_string();
        let list_prefix_key = unsafe { NSAccessibilityListItemPrefixTextAttribute }.to_string();
        let list_index_key = unsafe { NSAccessibilityListItemIndexTextAttribute }.to_string();
        let link_key = unsafe { NSAccessibilityLinkTextAttribute }.to_string();
        let attachment_key = unsafe { NSAccessibilityAttachmentTextAttribute }.to_string();
        let mut runs = Vec::new();
        let mut retained_bytes = source.len();
        let mut index = 0;
        while index < attributed.char_len() {
            text_context.references.check()?;
            if runs.len() >= 65536 {
                return Err(Error::action("Attributed run count exceeds bound"));
            }
            let mut range = core_foundation::base::CFRange {
                location: 0,
                length: 0,
            };
            let raw = unsafe {
                CFAttributedStringGetAttributes(attributed.as_concrete_TypeRef(), index, &mut range)
            };
            if raw.is_null() || range.length <= 0 || range.location != index {
                return Err(Error::action("Invalid AX attributed run"));
            }
            let attrs = unsafe { CFDictionary::<CFString, CFType>::wrap_under_get_rule(raw) };
            let attr = |name: &str| attrs.find(CFString::new(name)).map(|v| (*v).clone());
            let number = |name: &str| {
                attr(name)
                    .and_then(|v| v.downcast::<CFNumber>())
                    .and_then(|n| n.to_i64())
                    .unwrap_or(0)
            };
            let boolean = |name: &str| {
                attr(name).is_some_and(|v| {
                    v.downcast::<CFBoolean>().is_some_and(bool::from)
                        || v.downcast::<CFNumber>()
                            .and_then(|n| n.to_i64())
                            .is_some_and(|n| n != 0)
                })
            };
            let font = attr("AXFont")
                .and_then(|v| v.downcast::<CFDictionary>())
                .map(|d| unsafe {
                    CFDictionary::<CFString, CFType>::wrap_under_get_rule(d.as_concrete_TypeRef())
                });
            let traits = font.as_ref().map(font_traits).unwrap_or(0);
            let font_bold = font
                .as_ref()
                .and_then(|font| font.find(CFString::new("AXFontBold")))
                .is_some_and(|v| v.downcast::<CFBoolean>().is_some_and(bool::from));
            let link = attr(&link_key)
                .map(|value| text_context.references.resolve(value))
                .transpose()?;
            let attachment = attr(&attachment_key)
                .map(|value| text_context.references.resolve(value))
                .transpose()?;
            let deferred_link = link.and_then(|metadata| {
                metadata
                    .url
                    .map(|url| (url, metadata.role.as_deref() == Some("AXImage")))
            });
            let attachment_url = attachment.as_ref().and_then(|metadata| {
                (metadata.role.as_deref() == Some("AXImage"))
                    .then(|| metadata.url.clone())
                    .flatten()
            });
            let mut style = crate::rich_text::TextStyle {
                bold: traits & 2 != 0 || font_bold,
                italic: traits & 1 != 0,
                underline: number("AXUnderline") > 0
                    || attr("AXUnderline")
                        .and_then(|v| v.downcast::<CFBoolean>())
                        .is_some_and(bool::from),
                strikethrough: boolean("AXStrikethrough"),
                superscript: number("AXSuperscript").signum() as i8,
                attachment: attachment.map(|metadata| crate::rich_text::TextAttachment {
                    role_description: metadata.role_description,
                    description: metadata.description,
                    image_url: None,
                }),
                blockquote: number("AXBlockQuoteLevel").max(0) as usize,
                ..Default::default()
            };
            if let Some(names) = attr("AXStyleName").and_then(|v| v.downcast::<CFString>()) {
                style.accessibility_style_names(&names.to_string());
            }
            if let (Some(level), Some(prefix), Some(item_index)) = (
                attr(&list_level_key)
                    .and_then(|v| v.downcast::<CFNumber>())
                    .and_then(|v| v.to_i64()),
                attr(&list_prefix_key).and_then(|v| v.downcast::<CFAttributedString>()),
                attr(&list_index_key).and_then(|v| v.downcast::<CFNumber>()),
            ) {
                let level = usize::try_from(level)
                    .map_err(|_| Error::action("AX list level is negative"))?;
                let raw = unsafe { CFAttributedStringGetString(prefix.as_concrete_TypeRef()) };
                if raw.is_null() {
                    return Err(Error::action("AX list prefix has no string"));
                }
                let prefix = unsafe { CFString::wrap_under_get_rule(raw) };
                if prefix.char_len() > 16384 {
                    return Err(Error::action("AX list prefix exceeds length bound"));
                }
                let prefix = prefix.to_string();
                // CFNumber and NSNumber are toll-free bridged. Preserve the
                // provider's stringValue, as the original does, for ordered markers.
                let item_index: &NSNumber = unsafe { &*item_index.as_CFTypeRef().cast() };
                style.list = Some(crate::rich_text::ListStyle::from_accessibility_index_label(
                    level,
                    item_index.integerValue() as i64,
                    &item_index.stringValue().to_string(),
                    &prefix,
                ));
            }
            let run = super::render::DeferredRun {
                run: crate::rich_text::AttributedRun {
                    range: crate::selection::TextRange {
                        location: index as usize,
                        length: range.length as usize,
                    },
                    style,
                },
                link: deferred_link,
                attachment_url,
            };
            retained_bytes = retained_bytes.saturating_add(run.retained_bytes());
            if retained_bytes > 16 * 1024 * 1024 {
                return Err(Error::action("Attributed runs exceed retained-input bound"));
            }
            runs.push(run);
            index = index
                .checked_add(range.length)
                .ok_or_else(|| Error::action("Attributed run overflow"))?;
        }
        Ok(Some(super::render::AttributedInput {
            source: source.to_owned(),
            runs,
        }))
    }
    fn actions(&self) -> Result<Vec<String>> {
        let mut raw = ptr::null();
        ax_ok(unsafe { AXUIElementCopyActionNames(self.ptr(), &mut raw) })?;
        if raw.is_null() {
            return Err(Error::action("AX action names returned no array"));
        }
        let value = unsafe { CFType::wrap_under_create_rule(raw as _) };
        let array = value
            .downcast::<CFArray>()
            .ok_or_else(|| Error::action("AX action names has invalid array type"))?;
        if array.len() > 5000 {
            return Err(Error::action("AX action count exceeds collection bounds"));
        }
        array
            .get_all_values()
            .into_iter()
            .map(|p| {
                if p.is_null() {
                    return Err(Error::action("AX action name is null"));
                }
                unsafe { CFType::wrap_under_get_rule(p) }
                    .downcast::<CFString>()
                    .map(|s| s.to_string())
                    .ok_or_else(|| Error::action("AX action name is not a string"))
            })
            .collect()
    }
}
fn ax_ok(code: i32) -> Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(Error::action(format!(
            "Accessibility operation failed ({code})"
        )))
    }
}
fn trusted() -> Result<()> {
    if unsafe { AXIsProcessTrusted() } {
        Ok(())
    } else {
        Err(Error::new(
            -32003,
            "Grant Accessibility permission to this Rust executable or its launching terminal in System Settings",
        ))
    }
}
fn pump(duration: Duration) {
    let started = Instant::now();
    unsafe { CFRunLoop::run_in_mode(kCFRunLoopDefaultMode, duration, true) };
    // A run loop with no registered source returns immediately. Preserve the
    // requested poll interval instead of busy-spinning through timeout windows.
    if let Some(remaining) = duration.checked_sub(started.elapsed()) {
        std::thread::sleep(remaining);
    }
}
fn source() -> Result<CGEventSource> {
    CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .map_err(|_| Error::action("Cannot allocate event source"))
}

fn text_events(source: &CGEventSource, character: char) -> Result<[CGEvent; 2]> {
    let make = |down| -> Result<CGEvent> {
        let event = CGEvent::new_keyboard_event(source.clone(), 0, down)
            .map_err(|_| Error::action("Cannot allocate text event"))?;
        event.set_flags(CGEventFlags::empty());
        event.set_integer_value_field(EventField::KEYBOARD_EVENT_AUTOREPEAT, 0);
        event.set_string(&character.to_string());
        Ok(event)
    };
    // Both allocations must succeed before posting either half of the pair.
    Ok([make(true)?, make(false)?])
}

struct WindowContext {
    window: Ax,
    frame: Option<[f64; 4]>,
    window_id: Option<u32>,
}
pub struct MacDesktop {
    handles: BTreeMap<String, Ax>,
    audio: super::audio::Audio,
    next: u64,
    monitors: BTreeMap<i32, Monitor>,
    contexts: BTreeMap<i32, WindowContext>,
    screenshot_geometry: BTreeMap<i32, super::screenshot::Publication>,
    screenshot_configuration: super::screenshot::Configuration,
    monitor_errors: BTreeMap<i32, String>,
    needs_settle: std::collections::BTreeSet<i32>,
    source_warnings: BTreeMap<i32, Vec<String>>,
}
impl Default for MacDesktop {
    fn default() -> Self {
        Self::new()
    }
}
impl MacDesktop {
    pub fn new() -> Self {
        Self {
            handles: BTreeMap::new(),
            audio: Default::default(),
            next: 0,
            monitors: BTreeMap::new(),
            contexts: BTreeMap::new(),
            screenshot_geometry: BTreeMap::new(),
            screenshot_configuration: Default::default(),
            monitor_errors: BTreeMap::new(),
            needs_settle: Default::default(),
            source_warnings: Default::default(),
        }
    }
    /// Trusted Rust configuration only; the public Sky facade has no format keys.
    pub fn with_screenshot_configuration(configuration: super::screenshot::Configuration) -> Self {
        Self {
            screenshot_configuration: configuration,
            ..Self::new()
        }
    }
    fn snapshot_native(&mut self, app: &App) -> Result<Node> {
        self.source_warnings.remove(&app.pid);
        self.ensure_monitor(app)?;
        if NSRunningApplication::runningApplicationWithProcessIdentifier(app.pid)
            .is_some_and(|a| !a.isActive())
        {
            Self::focus(app)?;
            self.needs_settle.insert(app.pid);
        }
        self.settle(app);
        self.ensure_monitor(app)?;
        let window = self.root(app)?;
        if self.contexts.get(&app.pid).is_some_and(|prior|
            unsafe { CFEqual(prior.window.0.as_CFTypeRef(), window.0.as_CFTypeRef()) } == 0
                || prior.frame != window.frame()) {
            self.invalidate_screenshot(app);
        }
        let context = WindowContext {
            frame: window.frame(),
            window_id: window.text("AXWindowNumber").and_then(|n| n.parse().ok()),
            window: window.clone(),
        };
        let menu = self
            .monitors
            .get(&app.pid)
            .and_then(|m| m.menu())
            .filter(|m| {
                m.text("AXRole").is_some_and(|r| {
                    matches!(r.as_str(), "AXMenu" | "AXMenuBarItem" | "AXMenuItem")
                })
            });
        let is_menu = menu.is_some();
        let root = menu.unwrap_or(window);
        let text_deadline = Instant::now() + Duration::from_secs(10);
        let mut text_context = TextContext {
            references: references::ReferenceCache::new(app.pid, text_deadline),
            attributed: BTreeMap::new(),
            retained_bytes: 0,
            pid: app.pid,
            deadline: text_deadline,
            warnings: Vec::new(),
        };
        let mut result = self.capture(
            app.pid,
            root,
            &mut vec![],
            &mut 0,
            Instant::now() + Duration::from_secs(10),
        );
        if let Ok(root) = &mut result {
            if !is_menu && let Some(menu) = self.application(app)?.element_checked("AXMenuBar")? {
                let mut menu_node = self.capture(
                    app.pid,
                    menu,
                    &mut vec![],
                    &mut 0,
                    Instant::now() + Duration::from_secs(2),
                )?;
                for child in &mut menu_node.children {
                    child.children.clear();
                }
                root.children.push(menu_node);
            }
            let focus = if let Some(focused) = self
                .application(app)?
                .element_checked("AXFocusedUIElement")?
            {
                let identity = self.identity(app.pid, focused.clone());
                let focus = if let Some(node) = root.by_identity(&identity) {
                    node.clone()
                } else {
                    self.capture(
                        app.pid,
                        focused,
                        &mut vec![],
                        &mut 0,
                        Instant::now() + Duration::from_secs(2),
                    )?
                };
                Some(Box::new(focus))
            } else {
                None
            };
            *root = super::render::prepare_full_ui(
                root.clone(),
                focus,
                &app.id,
                &mut super::url::Shortener::default(),
                &mut |node, urls| self.prepare_attributed_node(node, urls, &mut text_context),
            )?;
            let warnings = self.source_warnings.entry(app.pid).or_default();
            warnings.extend(
                text_context
                    .warnings
                    .into_iter()
                    .take(64usize.saturating_sub(warnings.len())),
            );
            if let Some(monitor) = self.monitors.get_mut(&app.pid) {
                monitor.acknowledge();
            }
            self.contexts.insert(app.pid, context);
        }
        if let Ok(root) = &result {
            let mut nodes = vec![];
            root.walk(&mut nodes);
            if let Some(focus) = &root.focus_tree {
                focus.walk(&mut nodes);
            }
            let live: std::collections::HashSet<_> =
                nodes.into_iter().map(|n| n.identity.as_str()).collect();
            let prefix = format!("ax:{}:", app.pid);
            self.handles
                .retain(|id, _| !id.starts_with(&prefix) || live.contains(id.as_str()));
        }
        self.ensure_monitor(app)?;
        result
    }
    fn identity(&mut self, pid: i32, ax: Ax) -> String {
        if let Some((id, _)) = self
            .handles
            .iter()
            .find(|(_, old)| unsafe { CFEqual(old.0.as_CFTypeRef(), ax.0.as_CFTypeRef()) } != 0)
        {
            return id.clone();
        }
        let id = format!("ax:{pid}:{}", self.next);
        self.next += 1;
        self.handles.insert(id.clone(), ax);
        id
    }
    fn handle(&self, id: &str) -> Result<&Ax> {
        self.handles
            .get(id)
            .ok_or_else(|| Error::action("Unknown native element identity"))
    }
    fn application(&self, app: &App) -> Result<Ax> {
        trusted()?;
        let running = NSRunningApplication::runningApplicationWithProcessIdentifier(app.pid)
            .ok_or_else(|| Error::action("Application terminated"))?;
        if running.isTerminated()
            || running
                .bundleIdentifier()
                .is_none_or(|id| id.to_string() != app.id)
        {
            return Err(Error::action(
                "Application terminated or process identity changed",
            ));
        }
        let raw = unsafe { AXUIElementCreateApplication(app.pid) };
        if raw.is_null() {
            return Err(Error::action("Cannot create AX application"));
        }
        let ax = Ax(unsafe { CFType::wrap_under_create_rule(raw as _) });
        ax_ok(unsafe { AXUIElementSetMessagingTimeout(ax.ptr(), 0.25) })?;
        Ok(ax)
    }
    fn root(&self, app: &App) -> Result<Ax> {
        let ax = self.application(app)?;
        if let Some(window) = ax.element_checked("AXFocusedWindow")? {
            return Ok(window);
        }
        if let Some(window) = ax.element_checked("AXMainWindow")? {
            return Ok(window);
        }
        Ok(ax.elements_checked("AXWindows")?.pop().unwrap_or(ax))
    }
    fn ensure_monitor(&mut self, app: &App) -> Result<()> {
        if !self.monitors.contains_key(&app.pid) && !self.monitor_errors.contains_key(&app.pid) {
            let root = self.application(app)?;
            match Monitor::new(app.pid, root) {
                Ok(monitor) => {
                    self.monitors.insert(app.pid, monitor);
                }
                Err(error) => {
                    self.monitor_errors.insert(app.pid, error.message);
                }
            }
        }
        if let Some(monitor) = self.monitors.get(&app.pid) {
            monitor.check()?;
        }
        Ok(())
    }
    fn settle(&mut self, app: &App) {
        if !self.needs_settle.remove(&app.pid) {
            return;
        }
        let started = Instant::now();
        loop {
            let now = Instant::now();
            let quiet = self.monitors.get(&app.pid).map_or_else(
                || now.duration_since(started) >= Duration::from_millis(250),
                |m| m.quiet(now, Duration::from_millis(250), started),
            );
            if quiet || now.duration_since(started) >= Duration::from_secs(2) {
                break;
            }
            pump(Duration::from_millis(10));
        }
    }
    pub fn observation_diagnostics(&self, pid: i32) -> serde_json::Value {
        match self.monitors.get(&pid) {
            Some(m) => {
                serde_json::json!({"observer":true,"epoch":m.epoch(),"subscriptionErrors":m.failures,"callbackError":m.check().err().map(|e| e.message),"capture":"fresh","textSourceWarnings":self.source_warnings.get(&pid)})
            }
            None => {
                serde_json::json!({"observer":false,"error":self.monitor_errors.get(&pid),"capture":"fresh","textSourceWarnings":self.source_warnings.get(&pid)})
            }
        }
    }
    fn capture(
        &mut self,
        pid: i32,
        ax: Ax,
        ancestors: &mut Vec<String>,
        count: &mut usize,
        deadline: Instant,
    ) -> Result<Node> {
        if Instant::now() >= deadline {
            return Err(Error::action("AX capture deadline exceeded"));
        }
        *count += 1;
        if *count > 5000 || ancestors.len() >= 100 {
            return Err(Error::action("AX capture exceeded node/depth limit"));
        }
        let id = self.identity(pid, ax.clone());
        if ancestors.contains(&id) {
            return Err(Error::action("Cyclic AX children"));
        }
        ancestors.push(id.clone());
        let role = ax
            .get("AXRole")?
            .and_then(|v| v.downcast::<CFString>())
            .ok_or_else(|| Error::action("AX root/child is no longer accessible"))?
            .to_string();
        // Full-UI collection uses batches of 100; a menu bar contributes only
        // immediate menu items. An explicitly opened menu remains traversable.
        let shallow_menu_child = ancestors.len() > 1
            && ancestors
                .first()
                .and_then(|id| self.handles.get(id))
                .and_then(|a| a.text("AXRole"))
                .as_deref()
                == Some("AXMenuBar");
        let children = if shallow_menu_child {
            Vec::new()
        } else {
            ax.children()?
        };
        let mut prepared = None;
        let value = if matches!(role.as_str(), "AXTextArea" | "AXTextField" | "AXStaticText") {
            let mut reader = NativeTextReader::new(&ax, pid, deadline)?;
            let value = reader.get_optional("AXValue")?;
            let text = if value
                .as_ref()
                .is_some_and(|v| v.downcast::<CFString>().is_some())
            {
                let source = reader
                    .decode_string(value, "AXValue", super::text_source::MAX_SOURCE_UNITS)?
                    .unwrap_or_default();
                // Public AXUIElementCopyAttributeValue returns the complete
                // value without an AXPartialValue truncation descriptor.
                let result = super::text_source::prepare(
                    &role,
                    super::text_source::PartialValue {
                        text: source,
                        truncation: None,
                    },
                    &mut reader,
                )?;
                let text = Some(result.text.clone());
                prepared = Some(result);
                text
            } else {
                value
                    .and_then(|v| v.downcast::<CFNumber>())
                    .and_then(|n| n.to_f64())
                    .map(|n| n.to_string())
            };
            let warnings = self.source_warnings.entry(pid).or_default();
            warnings.extend(
                reader
                    .warnings
                    .into_iter()
                    .take(64usize.saturating_sub(warnings.len())),
            );
            text
        } else {
            ax.text("AXValue")
        };
        let mut node = Node {
            identity: id,
            role: role.clone(),
            subrole: ax.text("AXSubrole"),
            role_description: ax.text("AXRoleDescription"),
            title: ax.text("AXTitle"),
            description: ax.text("AXDescription"),
            value,
            truncation_range: prepared.as_ref().and_then(|p| p.range),
            value_description: ax.text("AXValueDescription"),
            placeholder: ax.text("AXPlaceholderValue"),
            help: ax.text("AXHelp"),
            identifier: ax.text("AXIdentifier"),
            url: ax.text("AXURL"),
            enabled: ax.boolean("AXEnabled").unwrap_or(true),
            focused: ax.boolean("AXFocused").unwrap_or(false),
            selected: ax.boolean("AXSelected").unwrap_or(false),
            selected_text: ax.text("AXSelectedText"),
            numeric_value: matches!(
                role.as_str(),
                "AXSlider" | "AXScrollBar" | "AXValueIndicator"
            ) && ax
                .get("AXValue")?
                .is_some_and(|value| value.downcast::<CFNumber>().is_some()),
            selectable: ax.settable("AXSelected"),
            focusable: ax.settable("AXFocused"),
            title_for: ax
                .title_relations()?
                .into_iter()
                .map(|a| self.identity(pid, a))
                .collect(),
            window_id: ax.text("AXWindowNumber").and_then(|s| s.parse().ok()),
            settable: prepared.as_ref().is_none_or(|p| p.range.is_none()) && ax.settable("AXValue"),
            frame: ax.frame(),
            actions: ax
                .actions()?
                .into_iter()
                .filter(|a| a != "AXPress")
                .collect(),
            ..Default::default()
        };
        if let Some(prepared) = prepared {
            prepared.apply_to(&mut node);
        }
        for child in children {
            node.children
                .push(self.capture(pid, child, ancestors, count, deadline)?);
        }
        ancestors.pop();
        Ok(node)
    }
    fn prepare_attributed_node(
        &self,
        node: &mut Node,
        urls: &mut super::url::Shortener,
        context: &mut TextContext,
    ) -> Result<()> {
        context.references.check()?;
        let Some(source) = node.value.as_deref() else {
            return Ok(());
        };
        if !context.attributed.contains_key(&node.identity) {
            let ax = self
                .handles
                .get(&node.identity)
                .ok_or_else(|| Error::action("Attributed source identity is no longer retained"))?;
            let mut reader = NativeTextReader::new(ax, context.pid, context.deadline)?;
            let allowed = super::text_source::attributed_allowed(None, &mut reader)?;
            let input = if allowed {
                ax.attributed(source, node.truncation_range, context)?
            } else {
                None
            };
            context.warnings.extend(
                reader
                    .warnings
                    .into_iter()
                    .take(64usize.saturating_sub(context.warnings.len())),
            );
            let bytes = node
                .identity
                .len()
                .saturating_add(source.len())
                .saturating_add(input.as_ref().map_or(0, |input| input.retained_bytes()));
            context.retained_bytes = context.retained_bytes.saturating_add(bytes);
            if context.retained_bytes > 16 * 1024 * 1024 || context.attributed.len() >= 5000 {
                return Err(Error::action(
                    "Attributed preparation exceeds observation input bound",
                ));
            }
            context.attributed.insert(
                node.identity.clone(),
                AttributedEntry {
                    source: source.to_owned(),
                    range: node.truncation_range,
                    input,
                },
            );
        }
        if let Some(entry) = context.attributed.get(&node.identity)
            && let Some(input) = entry.input_for(source, node.truncation_range)?
        {
            input.render_into(node, urls)?;
        }
        context.references.check()
    }
    fn focus(app: &App) -> Result<()> {
        let application = NSRunningApplication::runningApplicationWithProcessIdentifier(app.pid)
            .ok_or_else(|| Error::action("Application terminated"))?;
        if !application.isActive()
            && !application.activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows)
        {
            return Err(Error::action("Cannot activate target app"));
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if application.isTerminated() {
                return Err(Error::action("Application terminated during activation"));
            }
            if application.isActive()
                && NSWorkspace::sharedWorkspace()
                    .frontmostApplication()
                    .is_some_and(|front| front.processIdentifier() == app.pid)
            {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(Error::action("Target application did not become active"));
            }
            pump(Duration::from_millis(10));
        }
    }
    fn location(&self, app: &App, target: &Target) -> Result<[f64; 2]> {
        match target {
            Target::Point { point } => self.screen_point(app, *point),
            Target::Element { identity } => self
                .handle(identity)?
                .frame()
                .map(|[x, y, w, h]| [x + w / 2., y + h / 2.])
                .ok_or_else(|| Error::action("No frame for element")),
        }
    }
    fn current_context(&self, app: &App) -> Result<WindowContext> {
        self.current_context_with_geometry(app, false)
    }
    fn current_context_with_geometry(
        &self,
        app: &App,
        require_geometry: bool,
    ) -> Result<WindowContext> {
        let window = self.root(app)?;
        // Element scrolls can use their own bounds without window geometry.
        // Screenshot authority requires a checked read with its specific error.
        let frame = context_frame(
            require_geometry,
            || window.frame(),
            || window.checked_frame(),
        )?;
        if let Some(previous) = self.contexts.get(&app.pid)
            && (unsafe { CFEqual(previous.window.0.as_CFTypeRef(), window.0.as_CFTypeRef()) } == 0
                || previous.frame != frame)
        {
            return Err(Error::action(
                "Window context changed; query get_app_state before using screenshot coordinates",
            ));
        }
        Ok(WindowContext {
            window_id: window.text("AXWindowNumber").and_then(|n| n.parse().ok()),
            window,
            frame,
        })
    }
    fn screen_point(&self, app: &App, point: [f64; 2]) -> Result<[f64; 2]> {
        let frame = self
            .current_context_with_geometry(app, true)?
            .frame
            .ok_or_else(|| Error::action("No window frame for screenshot coordinates"))?;
        self.screenshot_geometry
            .get(&app.pid)
            .ok_or_else(|| {
                Error::action("Query get_app_state before using screenshot coordinates")
            })?
            .current(frame, &super::screenshot::macos::displays()?)?
            .screen_point(point)
    }
    fn capture_screenshot(&mut self, app: &App, publish_geometry: bool) -> Result<Image> {
        let receipt =
            publish_geometry.then(|| self.screenshot_geometry.entry(app.pid).or_default().begin());
        let result = (|| {
            self.ensure_monitor(app)?;
            self.settle(app);
            let context = self.current_context_with_geometry(app, true)?;
            let frame = context
                .frame
                .ok_or_else(|| Error::action("No window frame for screenshot"))?;
            let displays = super::screenshot::macos::displays()?;
            let geometry = super::screenshot::Geometry::new(
                frame,
                displays.scale_for_window(frame)?,
                self.screenshot_configuration,
            )?;
            let image = capture_window(
                app.pid,
                frame,
                context.window_id,
                geometry,
                self.screenshot_configuration.encoding,
            )?;
            let after = self.root(app)?;
            check_screenshot_context(
                unsafe { CFEqual(context.window.0.as_CFTypeRef(), after.0.as_CFTypeRef()) } != 0,
                frame,
                || after.checked_frame(),
                &displays,
                super::screenshot::macos::displays,
            )?;
            self.ensure_monitor(app)?;
            if let Some(receipt) = receipt {
                if !self.contexts.contains_key(&app.pid) {
                    return Err(Error::action(
                        "Screenshot observation requires a successful AX observation",
                    ));
                }
                self.screenshot_geometry
                    .entry(app.pid)
                    .or_default()
                    .commit(receipt, geometry, displays)?;
            }
            Ok(image)
        })();
        if result.is_err() {
            self.invalidate_screenshot(app);
        }
        result
    }
    fn key(app: &App, input: &str) -> Result<()> {
        let keys = super::keys::parse(input).map_err(Error::from)?;
        let mut factory = KeyEventFactory { source: source()? };
        // Construct the whole sequence before dispatch. A later allocation
        // failure drops all owned events without sending any earlier chord.
        let events = super::keys::prepare_events(&keys, &mut factory)?;
        for event in events {
            event.post_to_pid(app.pid);
        }
        Ok(())
    }
    fn mouse(
        &self,
        app: &App,
        position: [f64; 2],
        kind: CGEventType,
        button: CGMouseButton,
        count: u32,
    ) -> Result<()> {
        // The WindowServer performs pointer hit testing for the public event
        // stream. Posting a mouse event directly to a PID does not provide the
        // window-relative location AppKit needs and can hit another control.
        // Releases must still be sent if focus changes during a drag.
        if !matches!(
            kind,
            CGEventType::LeftMouseUp | CGEventType::RightMouseUp | CGEventType::OtherMouseUp
        ) {
            self.check_pointer_target(app, position)?;
        }
        let event = CGEvent::new_mouse_event(
            source()?,
            kind,
            CGPoint::new(position[0], position[1]),
            button,
        )
        .map_err(|_| Error::action("Cannot allocate mouse event"))?;
        event.set_integer_value_field(EventField::MOUSE_EVENT_CLICK_STATE, count as i64);
        event.post(CGEventTapLocation::HID);
        Ok(())
    }

    fn check_pointer_target(&self, app: &App, position: [f64; 2]) -> Result<()> {
        self.current_context(app)?;
        if !NSWorkspace::sharedWorkspace()
            .frontmostApplication()
            .is_some_and(|front| front.processIdentifier() == app.pid)
        {
            return Err(Error::action(
                "Target application lost activation before pointer input",
            ));
        }
        let system = unsafe { AXUIElementCreateSystemWide() };
        if system.is_null() {
            return Err(Error::action("Cannot inspect pointer target"));
        }
        let system = unsafe { CFType::wrap_under_create_rule(system.cast()) };
        let mut target = ptr::null_mut();
        ax_ok(unsafe {
            AXUIElementCopyElementAtPosition(
                system.as_CFTypeRef().cast_mut().cast(),
                position[0] as f32,
                position[1] as f32,
                &mut target,
            )
        })?;
        if target.is_null() {
            return Err(Error::action("No accessibility target at pointer location"));
        }
        let target = unsafe { CFType::wrap_under_create_rule(target.cast()) };
        let mut pid = 0;
        ax_ok(unsafe { AXUIElementGetPid(target.as_CFTypeRef().cast_mut().cast(), &mut pid) })?;
        if pid != app.pid {
            return Err(Error::action(
                "Pointer target is covered by another application; observe again",
            ));
        }
        Ok(())
    }
}
impl Desktop for MacDesktop {
    fn app_specific_instructions(&mut self, app: &App) -> Option<String> {
        apps::instructions(app)
    }
    fn app_policy_target(&mut self, identifier: &str) -> Result<App> {
        let running = self.apps()?;
        let mut matches = running.into_iter().filter(|app| {
            app.id == identifier
                || app.path == identifier
                || app.name == identifier
                || app.pid.to_string() == identifier
        });
        if let Some(app) = matches.next() {
            if matches.next().is_some() {
                return Err(Error::action("Ambiguous application identifier"));
            }
            return Ok(app);
        }
        let row = if identifier.starts_with('/') {
            apps::bundle(std::path::Path::new(identifier))
        } else {
            let rows = self.sky_apps()?;
            let mut matches = rows.as_array().unwrap().iter().filter(|app| {
                app["bundleIdentifier"] == identifier || app["displayName"] == identifier
            });
            let row = matches.next().cloned();
            if matches.next().is_some() {
                return Err(Error::action("Ambiguous application identifier"));
            }
            if row.is_some() {
                row
            } else {
                NSWorkspace::sharedWorkspace()
                    .URLForApplicationWithBundleIdentifier(&NSString::from_str(identifier))
                    .and_then(|url| url.path())
                    .and_then(|path| apps::bundle(std::path::Path::new(&path.to_string())))
            }
        }
        .ok_or_else(|| Error::action("Application not found"))?;
        Ok(App {
            id: row["bundleIdentifier"].as_str().unwrap().into(),
            name: row["displayName"].as_str().unwrap().into(),
            path: row["appPath"].as_str().unwrap().into(),
            pid: 0,
        })
    }
    fn sky_apps(&mut self) -> Result<serde_json::Value> {
        let mut roots = vec![
            "/Applications".into(),
            "/System/Applications".into(),
            "/System/Library/CoreServices/Applications".into(),
        ];
        if let Some(home) = std::env::var_os("HOME") {
            roots.push(std::path::PathBuf::from(home).join("Applications"));
        }
        apps::discover(&roots, self.apps()?)
    }
    fn apps(&mut self) -> Result<Vec<App>> {
        Ok(NSWorkspace::sharedWorkspace()
            .runningApplications()
            .iter()
            .filter_map(|a| {
                let id = a.bundleIdentifier()?.to_string();
                let path = a.bundleURL()?.path()?.to_string();
                Some(App {
                    id,
                    name: a.localizedName().map(|s| s.to_string()).unwrap_or_default(),
                    path,
                    pid: a.processIdentifier(),
                })
            })
            .collect())
    }
    fn bind(&mut self, identifier: &str) -> Result<App> {
        let find = |apps: Vec<App>| -> Result<Option<App>> {
            let matches: Vec<_> = apps
                .into_iter()
                .filter(|a| {
                    a.id == identifier
                        || a.path == identifier
                        || a.name == identifier
                        || a.pid.to_string() == identifier
                })
                .collect();
            match matches.len() {
                0 => Ok(None),
                1 => Ok(matches.into_iter().next()),
                _ => Err(Error::action("Ambiguous application identifier")),
            }
        };
        if let Some(app) = find(self.apps()?)? {
            return Ok(app);
        }
        let workspace = NSWorkspace::sharedWorkspace();
        let url = if identifier.starts_with('/')
            && identifier.ends_with(".app")
            && std::path::Path::new(identifier).is_dir()
        {
            Some(NSURL::fileURLWithPath(&NSString::from_str(identifier)))
        } else {
            workspace.URLForApplicationWithBundleIdentifier(&NSString::from_str(identifier))
        };
        let url = url.ok_or_else(|| Error::action("Application not found"))?;
        if !workspace.openURL(&url) {
            return Err(Error::action("Application launch failed"));
        }
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(5) {
            if let Some(app) = find(self.apps()?)? {
                return Ok(app);
            }
            pump(Duration::from_millis(50));
        }
        Err(Error::action("Application launch timed out"))
    }
    fn snapshot(&mut self, app: &App) -> Result<Node> {
        let result = self.snapshot_native(app);
        if result.is_err() {
            self.invalidate_screenshot(app);
        }
        result
    }
    fn action(&mut self, app: &App, action: Action) -> Result<()> {
        match &action {
            Action::Click { button, count, .. } if *button > 2 || !(1..=3).contains(count) => {
                return Err(Error::invalid("Invalid click button/count"));
            }
            Action::Scroll {
                direction, pages, ..
            } if !pages.is_finite()
                || *pages <= 0.
                || !["up", "down", "left", "right"].contains(&direction.as_str()) =>
            {
                return Err(Error::invalid("Invalid scroll direction/pages"));
            }
            Action::PressKey { key } => {
                super::keys::parse(key).map_err(Error::from)?;
            }
            Action::TypeText { text } | Action::SetValue { value: text, .. }
                if text.len() > 1024 * 1024 =>
            {
                return Err(Error::invalid("Text exceeds 1 MiB"));
            }
            Action::Paste { text, format } => {
                crate::rich_text::representations(text, format)?;
            }
            _ => (),
        }
        trusted()?;
        self.ensure_monitor(app)?;
        Self::focus(app)?;
        self.needs_settle.insert(app.pid);
        match action {
            Action::SetValue { identity, value } => {
                let ax = self.handle(&identity)?;
                if !ax.settable("AXValue") {
                    return Err(Error::action(
                        "Cannot set a value for an element that is not settable",
                    ));
                }
                let is_numeric = ax
                    .get("AXValue")?
                    .is_some_and(|v| v.downcast::<CFNumber>().is_some());
                let value = if is_numeric {
                    let number = value
                        .parse::<f64>()
                        .map_err(|_| Error::invalid("Expected numeric value"))?;
                    if !number.is_finite() {
                        return Err(Error::invalid("Expected finite numeric value"));
                    }
                    CFNumber::from(number).as_CFType()
                } else {
                    CFString::new(&value).as_CFType()
                };
                ax.set("AXValue", &value)?;
            }
            Action::SelectText { identity, range } => {
                let ax = self.handle(&identity)?;
                let can_focus = ax.settable("AXFocused");
                // AppKit field editors expose a writable selected range only
                // after their concrete text control becomes first responder.
                // Do not focus an arbitrary non-text element as a fallback.
                if !ax.settable("AXSelectedTextRange")
                    && (!can_focus
                        || !ax.text("AXRole").is_some_and(|role| {
                            matches!(
                                role.as_str(),
                                "AXTextField" | "AXTextArea" | "AXSearchField" | "AXComboBox"
                            )
                        }))
                {
                    return Err(Error::action("Selected text range is not settable"));
                }
                if can_focus {
                    ax.set("AXFocused", &CFBoolean::true_value().as_CFType())?;
                }
                let focus_deadline = Instant::now() + Duration::from_secs(1);
                while can_focus && ax.boolean("AXFocused") != Some(true) {
                    if Instant::now() >= focus_deadline {
                        return Err(Error::action("Text target did not acquire keyboard focus"));
                    }
                    pump(Duration::from_millis(10));
                }
                while !ax.settable("AXSelectedTextRange") {
                    if Instant::now() >= focus_deadline {
                        return Err(Error::action(
                            "Selected text range is not settable after focusing the text target",
                        ));
                    }
                    pump(Duration::from_millis(10));
                }
                let range = core_foundation::base::CFRange {
                    location: range
                        .location
                        .try_into()
                        .map_err(|_| Error::invalid("Selection offset overflow"))?,
                    length: range
                        .length
                        .try_into()
                        .map_err(|_| Error::invalid("Selection length overflow"))?,
                };
                let raw = unsafe { AXValueCreate(kAXValueTypeCFRange, &range as *const _ as _) };
                if raw.is_null() {
                    return Err(Error::action("Cannot allocate AX range"));
                }
                let value = unsafe { CFType::wrap_under_create_rule(raw as _) };
                ax.set("AXSelectedTextRange", &value)?;
                let deadline = Instant::now() + Duration::from_secs(1);
                loop {
                    let selected = ax.get("AXSelectedTextRange")?.ok_or_else(|| {
                        Error::action("Selected text range is unavailable after setting")
                    })?;
                    let mut actual = core_foundation::base::CFRange {
                        location: 0,
                        length: 0,
                    };
                    if selected.type_of() != unsafe { AXValueGetTypeID() }
                        || !unsafe {
                            AXValueGetValue(
                                selected.as_CFTypeRef() as _,
                                kAXValueTypeCFRange,
                                &mut actual as *mut _ as _,
                            )
                        }
                    {
                        return Err(Error::action("Selected text range has invalid native type"));
                    }
                    if actual.location == range.location && actual.length == range.length {
                        break;
                    }
                    if Instant::now() >= deadline {
                        return Err(Error::action(
                            "Text selection did not match the requested UTF-16 range",
                        ));
                    }
                    pump(Duration::from_millis(10));
                }
            }
            Action::Secondary { identity, action } => self.handle(&identity)?.perform(&action)?,
            Action::Click {
                target,
                button,
                count,
            } => {
                if let Target::Element { identity } = &target {
                    let ax = self.handle(identity)?;
                    if ax.boolean("AXEnabled") == Some(false) {
                        return Err(Error::action("Target is disabled"));
                    }
                    if button == 0 && count == 1 && ax.actions()?.iter().any(|a| a == "AXPress") {
                        ax.perform("AXPress")?;
                        pump(Duration::from_millis(50));
                        return self.ensure_monitor(app);
                    }
                }
                let p = self.location(app, &target)?;
                let (button, down, up) = match button {
                    0 => (
                        CGMouseButton::Left,
                        CGEventType::LeftMouseDown,
                        CGEventType::LeftMouseUp,
                    ),
                    1 => (
                        CGMouseButton::Right,
                        CGEventType::RightMouseDown,
                        CGEventType::RightMouseUp,
                    ),
                    _ => (
                        CGMouseButton::Center,
                        CGEventType::OtherMouseDown,
                        CGEventType::OtherMouseUp,
                    ),
                };
                for click in 1..=count {
                    self.mouse(app, p, down, button, click)?;
                    self.mouse(app, p, up, button, click)?;
                }
            }
            Action::Drag { from, to } => {
                let from = self.screen_point(app, from)?;
                let to = self.screen_point(app, to)?;
                self.mouse(
                    app,
                    from,
                    CGEventType::LeftMouseDown,
                    CGMouseButton::Left,
                    1,
                )?;
                let movement = (|| -> Result<()> {
                    for step in 1..=20 {
                        let t = step as f64 / 20.;
                        self.mouse(
                            app,
                            [
                                from[0] + t * (to[0] - from[0]),
                                from[1] + t * (to[1] - from[1]),
                            ],
                            CGEventType::LeftMouseDragged,
                            CGMouseButton::Left,
                            1,
                        )?;
                        pump(Duration::from_millis(5));
                    }
                    Ok(())
                })();
                let released =
                    self.mouse(app, to, CGEventType::LeftMouseUp, CGMouseButton::Left, 1);
                movement?;
                released?;
            }
            Action::PressKey { key } => Self::key(app, &key)?,
            Action::TypeText { text } => {
                if text.is_empty() {
                    return self.ensure_monitor(app);
                }
                // Drain the preceding command's observed state before deriving
                // an insertion expectation (notably a menu-backed Select All).
                self.settle(app);
                let application = self.application(app)?;
                let focused = application.element_checked("AXFocusedUIElement")?;
                let expectation = if let Some(focused) = &focused {
                    if focused.settable("AXValue") {
                        if let (Some(value), Some(range)) = (
                            focused
                                .get("AXValue")?
                                .and_then(|v| v.downcast::<CFString>())
                                .map(|v| v.to_string()),
                            focused.selected_range()?,
                        ) {
                            Some((
                                crate::selection::replace_utf16(&value, range, &text)?,
                                range
                                    .location
                                    .checked_add(text.encode_utf16().count())
                                    .ok_or_else(|| Error::invalid("Typed text caret overflow"))?,
                            ))
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };
                let check_focus = || -> Result<()> {
                    if !NSRunningApplication::runningApplicationWithProcessIdentifier(app.pid)
                        .is_some_and(|running| running.isActive())
                    {
                        return Err(Error::action(
                            "Target application lost activation during typing",
                        ));
                    }
                    if let Some(expected) = &focused {
                        let actual = application.element_checked("AXFocusedUIElement")?;
                        if !actual.is_some_and(|actual| unsafe {
                            CFEqual(actual.0.as_CFTypeRef(), expected.0.as_CFTypeRef()) != 0
                        }) {
                            return Err(Error::action(
                                "Keyboard focus changed during typing; remaining text was not sent",
                            ));
                        }
                    }
                    Ok(())
                };
                let deadline = Instant::now() + Duration::from_secs(10);
                // Unicode injection is a remote-control source, independent of
                // hardware modifier and held-key state. Keep its private state
                // alive for the whole sequence rather than recreating it per event.
                let text_source = CGEventSource::new(CGEventSourceStateID::Private)
                    .map_err(|_| Error::action("Cannot allocate private text event source"))?;
                for character in text.chars() {
                    check_focus()?;
                    if Instant::now() >= deadline {
                        return Err(Error::action(
                            "Typing deadline exceeded after partial input",
                        ));
                    }
                    let events = text_events(&text_source, character)?;
                    for event in events {
                        event.post_to_pid(app.pid);
                    }
                    // Event posting is asynchronous. Allow the target to process
                    // each pair before posting the next identical virtual key.
                    pump(Duration::from_millis(10));
                }
                // CGEventPostToPid has no acknowledgement. For an editable AX
                // text target, keep the private source alive until both text and
                // caret prove the insertion was applied. Never silently refocus
                // or retry after a user/application focus change.
                if let (Some(focused), Some((expected, caret))) = (&focused, expectation) {
                    loop {
                        check_focus()?;
                        let value = focused
                            .get("AXValue")?
                            .and_then(|v| v.downcast::<CFString>())
                            .map(|v| v.to_string());
                        let range = focused.selected_range()?;
                        if value.as_deref() == Some(expected.as_str())
                            && range
                                == Some(crate::selection::TextRange {
                                    location: caret,
                                    length: 0,
                                })
                        {
                            break;
                        }
                        if Instant::now() >= deadline {
                            return Err(Error::action(
                                "Typed text or caret did not reach the expected UTF-16 insertion before the deadline",
                            ));
                        }
                        pump(Duration::from_millis(10));
                    }
                } else {
                    pump(Duration::from_millis(250));
                    check_focus()?;
                }
                self.needs_settle.insert(app.pid);
            }
            Action::Scroll {
                target,
                direction,
                pages,
            } => {
                let context = self.current_context(app)?;
                let frame = match &target {
                    Target::Element { identity } => {
                        self.handle(identity)?.frame().or(context.frame)
                    }
                    Target::Point { .. } => context.frame,
                }
                .ok_or_else(|| Error::action("No scroll target bounds"))?;
                let [horizontal, vertical] = super::scroll::page_deltas(&direction, pages, frame)?;
                let position = self.location(app, &target)?;
                let event = CGEvent::new_scroll_event(source()?, 0, 1, vertical, horizontal, 0)
                    .map_err(|_| Error::action("Cannot allocate scroll event"))?;
                event.set_location(CGPoint::new(position[0], position[1]));
                if let Some(window_id) = context.window_id {
                    event.set_integer_value_field(
                        EventField::MOUSE_EVENT_WINDOW_UNDER_MOUSE_POINTER,
                        window_id as i64,
                    );
                    event.set_integer_value_field(EventField::MOUSE_EVENT_WINDOW_UNDER_MOUSE_POINTER_THAT_CAN_HANDLE_THIS_EVENT, window_id as i64);
                }
                self.mouse(
                    app,
                    position,
                    CGEventType::MouseMoved,
                    CGMouseButton::Left,
                    0,
                )?;
                self.check_pointer_target(app, position)?;
                event.post(CGEventTapLocation::HID);
            }
            Action::Paste { text, format } => {
                let item = crate::rich_text::representations(&text, &format)?;
                let mut board = MacPasteboard::new();
                let mut transaction = clipboard::Transaction::begin(&mut board, &[item])?;
                let operation = Self::key(app, "super+v")
                    .and_then(|_| transaction.wait(Duration::from_secs(2)));
                let restored = transaction.finish();
                operation?;
                restored?;
            }
        }
        pump(Duration::from_millis(50));
        self.ensure_monitor(app)
    }
    fn screenshot(&mut self, app: &App) -> Result<Image> {
        self.capture_screenshot(app, false)
    }
    fn screenshot_for_observation(&mut self, app: &App) -> Result<Image> {
        self.capture_screenshot(app, true)
    }
    fn invalidate_screenshot(&mut self, app: &App) {
        self.screenshot_geometry
            .entry(app.pid)
            .or_default()
            .invalidate();
    }
    fn audio(
        &mut self,
        method: &str,
        owner: &str,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        self.audio.execute(method, owner, params)
    }
    fn cancel_audio(&mut self, owner: &str) -> Result<()> {
        self.audio.end_session(owner)
    }
    fn end_session(&mut self, owner: &str) -> Result<()> {
        self.audio.end_session(owner)?;
        self.monitors.clear();
        self.contexts.clear();
        self.screenshot_geometry.clear();
        self.handles.clear();
        self.needs_settle.clear();
        self.monitor_errors.clear();
        Ok(())
    }
    fn diagnostics(&self, app: &App) -> serde_json::Value {
        self.observation_diagnostics(app.pid)
    }
    fn capabilities(&self) -> Vec<&'static str> {
        vec![
            "list_apps",
            "bind_app",
            "get_app_state",
            "click",
            "drag",
            "press_key",
            "type_text",
            "set_value",
            "select_text",
            "scroll",
            "perform_secondary_action",
            "paste",
            "get_screenshot",
            "audio.start",
            "audio.stop",
            "audio.status",
        ]
    }
}

struct KeyEventFactory {
    source: CGEventSource,
}
impl super::keys::EventFactory for KeyEventFactory {
    type Event = CGEvent;
    fn make(&mut self, spec: super::keys::EventSpec) -> Result<CGEvent> {
        use super::keys::EventKind;
        let event = match spec.kind {
            EventKind::FlagsChanged => {
                let event = CGEvent::new(self.source.clone())
                    .map_err(|_| Error::action("Cannot allocate flags event"))?;
                event.set_type(CGEventType::FlagsChanged);
                event
            }
            EventKind::KeyDown | EventKind::KeyUp => {
                let code = spec
                    .code
                    .ok_or_else(|| Error::action("Keyboard event has no key code"))?;
                CGEvent::new_keyboard_event(
                    self.source.clone(),
                    code,
                    spec.kind == EventKind::KeyDown,
                )
                .map_err(|_| Error::action("Cannot allocate key event"))?
            }
        };
        event.set_flags(CGEventFlags::from_bits_retain(spec.flags));
        Ok(event)
    }
    fn saved_flags(&mut self) -> Result<u64> {
        // The original samples CombinedSessionState after key-up allocation,
        // even though its one retained source is HIDSystemState.
        Ok(unsafe { CGEventSourceFlagsState(CGEventSourceStateID::CombinedSessionState) })
    }
}

#[derive(Default)]
struct ProviderState {
    item: Item,
    consumed: Arc<AtomicBool>,
}
define_class!(
    #[unsafe(super(NSObject))]
    #[name = "SkyreClipboardProvider"]
    #[ivars=ProviderState]
    struct DataProvider;
    unsafe impl NSObjectProtocol for DataProvider {}
    unsafe impl NSPasteboardItemDataProvider for DataProvider {
        #[unsafe(method(pasteboard:item:provideDataForType:))]
        fn provide(
            &self,
            _board: Option<&NSPasteboard>,
            item: &NSPasteboardItem,
            kind: &NSPasteboardType,
        ) {
            if let Some(bytes) = self.ivars().item.get(&kind.to_string())
                && item.setData_forType(&NSData::with_bytes(bytes), kind)
            {
                self.ivars().consumed.store(true, Ordering::Release);
            }
        }
    }
);
impl DataProvider {
    fn new(item: Item, consumed: Arc<AtomicBool>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(ProviderState { item, consumed });
        unsafe { msg_send![super(this), init] }
    }
}
struct MacPasteboard {
    board: Retained<NSPasteboard>,
    consumed: Arc<AtomicBool>,
    first: bool,
    providers: Vec<Retained<DataProvider>>,
}
impl MacPasteboard {
    fn new() -> Self {
        Self {
            board: NSPasteboard::generalPasteboard(),
            consumed: Arc::new(AtomicBool::new(false)),
            first: true,
            providers: vec![],
        }
    }
}
impl Pasteboard for MacPasteboard {
    fn generation(&self) -> i64 {
        self.board.changeCount() as i64
    }
    fn snapshot(&self) -> Result<Vec<Item>> {
        let mut result = vec![];
        let mut total = 0usize;
        if let Some(items) = self.board.pasteboardItems() {
            for item in items.iter() {
                let mut reps = Item::new();
                for kind in item.types().iter() {
                    let data = item.dataForType(&kind).ok_or_else(|| {
                        Error::action("Cannot preserve an unreadable clipboard representation")
                    })?;
                    total = total.saturating_add(data.length());
                    if total > 64 * 1024 * 1024 {
                        return Err(Error::action("Clipboard snapshot exceeds 64 MiB"));
                    }
                    reps.insert(kind.to_string(), data.to_vec());
                }
                result.push(reps);
            }
        }
        Ok(result)
    }
    fn install(&mut self, items: &[Item], expected: i64) -> Result<Option<i64>> {
        if self.generation() != expected {
            return Ok(None);
        }
        let previous = self.snapshot()?;
        let mut objects: Vec<Retained<NSPasteboardItem>> = vec![];
        let mut providers = vec![];
        for reps in items {
            let item = NSPasteboardItem::new();
            if self.first {
                let provider = DataProvider::new(reps.clone(), self.consumed.clone());
                let types: Vec<_> = reps.keys().map(|s| NSString::from_str(s)).collect();
                let types = NSArray::from_retained_slice(&types);
                if !item.setDataProvider_forTypes(ProtocolObject::from_ref(&*provider), &types) {
                    return Err(Error::action("Cannot install clipboard data provider"));
                }
                providers.push(provider);
            } else {
                for (kind, bytes) in reps {
                    if !item.setData_forType(&NSData::with_bytes(bytes), &NSString::from_str(kind))
                    {
                        return Err(Error::action("Cannot construct clipboard representation"));
                    }
                }
            }
            objects.push(item);
        }
        let objects: Vec<&ProtocolObject<dyn NSPasteboardWriting>> = objects
            .iter()
            .map(|item| ProtocolObject::from_ref(&**item))
            .collect();
        let objects = NSArray::from_slice(&objects);
        if self.generation() != expected {
            return Ok(None);
        }
        // AppKit has no atomic expected-count clear. Keep the caller's count
        // through all preparation and put this check immediately before clear.
        let count = self.board.clearContents();
        if self.board.changeCount() != count {
            return Err(Error::action("Clipboard changed during installation"));
        }
        if !items.is_empty() && !self.board.writeObjects(&objects) {
            let _ = write_eager(&self.board, &previous, count);
            return Err(Error::action("Clipboard write failed"));
        }
        if self.board.changeCount() != count {
            return Err(Error::action("Clipboard changed after installation"));
        }
        self.first = false;
        self.providers = providers;
        // clearContents returns the acquired ownership count. A later sample
        // alone could incorrectly adopt another writer as our transaction.
        Ok(Some(count as i64))
    }
    fn consumed(&self) -> bool {
        self.consumed.load(Ordering::Acquire)
    }
    fn poll(&mut self, duration: Duration) {
        pump(duration)
    }
}
fn write_eager(board: &NSPasteboard, items: &[Item], expected: isize) -> bool {
    if board.changeCount() != expected {
        return false;
    }
    let mut objects = Vec::new();
    for reps in items {
        let item = NSPasteboardItem::new();
        for (kind, bytes) in reps {
            if !item.setData_forType(&NSData::with_bytes(bytes), &NSString::from_str(kind)) {
                return false;
            }
        }
        objects.push(item);
    }
    let refs: Vec<&ProtocolObject<dyn NSPasteboardWriting>> = objects
        .iter()
        .map(|i| ProtocolObject::from_ref(&**i))
        .collect();
    let objects = NSArray::from_slice(&refs);
    // Rollback construction may invoke foreign providers/allocations. Never
    // treat a writer which arrived during that work as our new baseline.
    if board.changeCount() != expected {
        return false;
    }
    let count = board.clearContents();
    if board.changeCount() != count {
        return false;
    }
    (items.is_empty() || board.writeObjects(&objects)) && board.changeCount() == count
}
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventSourceFlagsState(state_id: CGEventSourceStateID) -> u64;
    pub(super) fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
}
fn capture_window(
    pid: i32,
    ax_frame: [f64; 4],
    window_id: Option<u32>,
    geometry: super::screenshot::Geometry,
    encoding: super::screenshot::Encoding,
) -> Result<Image> {
    if !unsafe { CGPreflightScreenCaptureAccess() } {
        return Err(Error::new(
            -32003,
            "Grant Screen Recording permission to this executable or launching terminal",
        ));
    }
    let (send, receive) = std::sync::mpsc::channel();
    let content_callback = RcBlock::new(
        move |content: *mut SCShareableContent, error: *mut NSError| {
            let fail = |message: String| {
                let _ = send.send(Err(Error::action(message)));
            };
            // SAFETY: ScreenCaptureKit guarantees callback pointers for the duration
            // of the invocation. No borrowed Cocoa objects leave this callback.
            unsafe {
                if let Some(error) = error.as_ref() {
                    fail(error.localizedDescription().to_string());
                    return;
                }
                let Some(content) = content.as_ref() else {
                    fail("No shareable content".into());
                    return;
                };
                let windows = content.windows();
                let matches: Vec<_> = windows
                    .iter()
                    .filter(|w| {
                        w.owningApplication().is_some_and(|a| a.processID() == pid)
                            && w.windowLayer() == 0
                            && window_id.is_none_or(|id| w.windowID() == id)
                            && (window_id.is_some() || {
                                let [x, y, width, height] = ax_frame;
                                let f = w.frame();
                                (f.origin.x - x).abs() < 2.
                                    && (f.origin.y - y).abs() < 2.
                                    && (f.size.width - width).abs() < 2.
                                    && (f.size.height - height).abs() < 2.
                            })
                    })
                    .collect();
                // The native AX tree represents one window. Match the known ID,
                // or require a unique frame match when no native ID is available.
                if matches.len() != 1 {
                    fail(format!(
                        "Expected one visible application window, found {}",
                        matches.len()
                    ));
                    return;
                }
                let filter = SCContentFilter::initWithDesktopIndependentWindow(
                    SCContentFilter::alloc(),
                    &matches[0],
                );
                let rect = filter.contentRect();
                let actual_frame = matches[0].frame();
                let actual_frame = [
                    actual_frame.origin.x,
                    actual_frame.origin.y,
                    actual_frame.size.width,
                    actual_frame.size.height,
                ];
                if actual_frame
                    .into_iter()
                    .zip(ax_frame)
                    .any(|(actual, expected)| {
                        !actual.is_finite() || (actual - expected).abs() > 0.01
                    })
                    || (rect.size.width - ax_frame[2]).abs() > 0.01
                    || (rect.size.height - ax_frame[3]).abs() > 0.01
                    || !rect.size.width.is_finite()
                    || !rect.size.height.is_finite()
                {
                    fail("Capture content does not match the observed window geometry".into());
                    return;
                }
                let config = SCStreamConfiguration::new();
                config.setWidth(geometry.pixels[0]);
                config.setHeight(geometry.pixels[1]);
                config.setShowsCursor(false);
                config.setIgnoreShadowsSingleWindow(true);
                let sender = send.clone();
                let callback = RcBlock::new(
                    move |image: *mut objc2_core_graphics::CGImage, error: *mut NSError| {
                        let result = if let Some(error) = error.as_ref() {
                            Err(Error::action(error.localizedDescription().to_string()))
                        } else if image.is_null() {
                            Err(Error::action("No screenshot image"))
                        } else {
                            super::screenshot::macos::encode(
                                image as *mut c_void,
                                encoding,
                                geometry.pixels,
                            )
                        };
                        let _ = sender.send(result);
                    },
                );
                SCScreenshotManager::captureImageWithFilter_configuration_completionHandler(
                    &filter,
                    &config,
                    Some(&callback),
                );
            }
        },
    );
    unsafe {
        SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(true,true,&content_callback)
    };
    let start = Instant::now();
    loop {
        match receive.try_recv() {
            Ok(result) => return result,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                return Err(Error::action("Screenshot callback disconnected"));
            }
            _ => {}
        }
        if start.elapsed() > Duration::from_secs(10) {
            return Err(Error::action("Screenshot timed out"));
        }
        pump(Duration::from_millis(10));
    }
}
pub fn permissions(request: bool) -> Result<serde_json::Value> {
    let accessibility = if request {
        let key = unsafe { CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt) };
        let options = CFDictionary::from_CFType_pairs(&[(
            key.as_CFType(),
            CFBoolean::true_value().as_CFType(),
        )]);
        unsafe { AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef()) }
    } else {
        unsafe { AXIsProcessTrusted() }
    };
    let screen_capture = if request && !unsafe { CGPreflightScreenCaptureAccess() } {
        unsafe { CGRequestScreenCaptureAccess() }
    } else {
        unsafe { CGPreflightScreenCaptureAccess() }
    };
    Ok(
        serde_json::json!({"accessibility":accessibility,"screenCapture":screen_capture,"requestIssued":request,"executable":std::env::current_exe()?.to_string_lossy(),"pid":std::process::id(),"note":"macOS chooses the responsible application shown in its permission dialog; a grant may require restarting this helper"}),
    )
}

#[cfg(test)]
mod screenshot_diagnostic_tests {
    use super::*;
    use crate::native::screenshot::{Displays, Screen};

    fn native_value<T>(kind: u32, value: &T) -> CFType {
        let raw = unsafe { AXValueCreate(kind, (value as *const T).cast()) };
        assert!(!raw.is_null());
        unsafe { CFType::wrap_under_create_rule(raw as _) }
    }
    fn values() -> (CFType, CFType) {
        (
            native_value(kAXValueTypeCGPoint, &CGPoint::new(-120., 48.)),
            native_value(kAXValueTypeCGSize, &CGSize::new(768., 1128.)),
        )
    }
    fn displays() -> Displays {
        Displays(vec![Screen {
            frame: [0., 0., 1920., 1200.],
            backing_scale: 2.,
        }])
    }

    #[test]
    fn checked_window_frame_decodes_actual_ax_values_without_ui() {
        let (position, size) = values();
        let frame = checked_ax_frame(|name| {
            Ok(Some(if name == "AXPosition" {
                position.clone()
            } else {
                assert_eq!(name, "AXSize");
                size.clone()
            }))
        })
        .unwrap();
        assert_eq!(frame, [-120., 48., 768., 1128.]);
    }

    #[test]
    fn checked_window_frame_preserves_read_failure_and_distinguishes_missing_or_malformed_values() {
        let (position, size) = values();
        let error = checked_ax_frame(|name| {
            if name == "AXPosition" {
                Ok(Some(position.clone()))
            } else {
                Err(Error::new(-10005, "AX error -25204"))
            }
        })
        .unwrap_err();
        assert_eq!(error.code, -10005);
        assert_eq!(error.message, "Cannot read AXSize: AX error -25204");
        let missing = checked_ax_frame(|_| Ok(None)).unwrap_err();
        assert_eq!(
            missing.message,
            "Window geometry attribute AXPosition is unavailable"
        );
        let wrong_type =
            checked_ax_frame(|_| Ok(Some(CFString::new("invalid").as_CFType()))).unwrap_err();
        assert_eq!(
            wrong_type.message,
            "Window geometry attribute AXPosition is not an AXValue"
        );
        let wrong_kind = checked_ax_frame(|_| Ok(Some(size.clone()))).unwrap_err();
        assert_eq!(
            wrong_kind.message,
            "Window AXPosition does not contain a CGPoint"
        );
        let missing_size =
            checked_ax_frame(|name| Ok((name == "AXPosition").then(|| position.clone())))
                .unwrap_err();
        assert_eq!(
            missing_size.message,
            "Window geometry attribute AXSize is unavailable"
        );
        let wrong_size_kind = checked_ax_frame(|_| Ok(Some(position.clone()))).unwrap_err();
        assert_eq!(
            wrong_size_kind.message,
            "Window AXSize does not contain a CGSize"
        );
    }

    #[test]
    fn optional_window_geometry_preserves_element_scroll_bounds_but_not_screenshot_authority() {
        let element_frame = [40., 50., 320., 240.];
        let unavailable = || checked_ax_frame(|_| Err(Error::new(-10005, "AX error -25204")));
        let window_frame = context_frame(
            false,
            || None,
            || panic!("An element scroll must keep the optional window read"),
        )
        .unwrap();
        let scroll_frame = Some(element_frame).or(window_frame).unwrap();
        assert_eq!(
            super::super::scroll::page_deltas("down", 1., scroll_frame).unwrap(),
            [0, -240]
        );
        let error = context_frame(
            true,
            || panic!("Screenshot authority must use the checked window read"),
            unavailable,
        )
        .unwrap_err();
        assert_eq!(error.code, -10005);
        assert_eq!(error.message, "Cannot read AXPosition: AX error -25204");
    }

    #[test]
    fn unreadable_post_capture_geometry_rejects_without_claiming_movement_or_reading_displays() {
        let error = check_screenshot_context(
            true,
            [-120., 48., 768., 1128.],
            || {
                Err(Error::new(
                    -10005,
                    "Cannot read AXPosition: AX error -25204",
                ))
            },
            &displays(),
            || panic!("A failed window read must stop before display collection"),
        )
        .unwrap_err();
        assert_eq!(error.code, -10005);
        assert_eq!(
            error.message,
            "Cannot verify window geometry after screenshot capture: Cannot read AXPosition: AX error -25204"
        );
        assert!(!error.message.contains("changed"));
    }

    #[test]
    fn screenshot_context_distinguishes_identity_frame_and_display_changes_in_original_order() {
        let frame = [-120., 48., 768., 1128.];
        let identity = check_screenshot_context(
            false,
            frame,
            || panic!("An identity mismatch must stop before frame collection"),
            &displays(),
            || panic!("An identity mismatch must stop before display collection"),
        )
        .unwrap_err();
        assert!(identity.message.starts_with("Window identity changed"));
        let moved = check_screenshot_context(
            true,
            frame,
            || Ok([-119., 48., 768., 1128.]),
            &displays(),
            || panic!("A frame mismatch must stop before display collection"),
        )
        .unwrap_err();
        assert!(moved.message.starts_with("Window frame changed"));
        assert!(moved.message.contains("-120.0") && moved.message.contains("-119.0"));
        let mut changed = displays();
        changed.0[0].backing_scale = 1.;
        let display =
            check_screenshot_context(true, frame, || Ok(frame), &displays(), || Ok(changed))
                .unwrap_err();
        assert!(display.message.starts_with("Display geometry changed"));
        check_screenshot_context(true, frame, || Ok(frame), &displays(), || Ok(displays()))
            .unwrap();
    }

    #[test]
    fn unreadable_post_capture_displays_reject_without_claiming_a_configuration_change() {
        let frame = [-120., 48., 768., 1128.];
        let error = check_screenshot_context(
            true,
            frame,
            || Ok(frame),
            &displays(),
            || Err(Error::new(-10005, "Display metadata unavailable")),
        )
        .unwrap_err();
        assert_eq!(error.code, -10005);
        assert_eq!(
            error.message,
            "Cannot verify display geometry after screenshot capture: Display metadata unavailable"
        );
    }
}

#[cfg(test)]
mod attributed_tests {
    use super::*;
    use crate::clipboard::Transaction;

    #[test]
    fn owned_named_pasteboard_receipt_lazy_consumption_restore_and_competitor() {
        // This test never calls generalPasteboard or MacPasteboard::new. The
        // daemon allocates a distinct named board containing only our bytes.
        struct ReleaseOwned(Retained<NSPasteboard>);
        impl Drop for ReleaseOwned {
            fn drop(&mut self) {
                // Public oneway selector omitted by the generated binding.
                let _: () = unsafe { msg_send![&*self.0, releaseGlobally] };
            }
        }
        let named = NSPasteboard::pasteboardWithUniqueName();
        let _release = ReleaseOwned(named.clone());
        let saved = vec![Item::from([
            (
                "public.utf8-plain-text".into(),
                "saved β🧪".as_bytes().to_vec(),
            ),
            ("org.skyre.synthetic".into(), vec![0, 255, 10]),
        ])];
        assert!(write_eager(&named, &saved, named.changeCount()));
        let mut provider = MacPasteboard {
            board: named.clone(),
            consumed: Arc::new(AtomicBool::new(false)),
            first: true,
            providers: vec![],
        };
        let temporary = vec![Item::from([(
            "public.utf8-plain-text".into(),
            "temporary 終🧪".as_bytes().to_vec(),
        )])];
        let before = named.changeCount();
        let mut tx = Transaction::begin(&mut provider, &temporary).unwrap();
        let acquired = named.changeCount();
        assert!(acquired > before);
        let objects = named.pasteboardItems().unwrap();
        let value = objects
            .objectAtIndex(0)
            .dataForType(&NSString::from_str("public.utf8-plain-text"))
            .unwrap();
        assert_eq!(value.to_vec(), temporary[0]["public.utf8-plain-text"]);
        tx.wait(Duration::from_millis(250)).unwrap();
        assert_eq!(named.changeCount(), acquired);
        assert!(tx.finish().unwrap());
        assert_eq!(provider.snapshot().unwrap(), saved);

        let expected = provider.generation();
        let receipt = provider.install(&temporary, expected).unwrap().unwrap();
        assert_eq!(receipt, named.changeCount() as i64);
        assert_eq!(provider.install(&saved, expected).unwrap(), None);
        assert_eq!(provider.snapshot().unwrap(), temporary);

        let tx = Transaction::begin(&mut provider, &temporary).unwrap();
        let competitor = vec![Item::from([(
            "public.utf8-plain-text".into(),
            b"owned synthetic competitor".to_vec(),
        )])];
        assert!(write_eager(&named, &competitor, named.changeCount()));
        assert!(!tx.finish().unwrap());
        assert_eq!(provider.snapshot().unwrap(), competitor);
    }

    #[test]
    fn attributed_cache_rejects_changed_local_source_and_global_range_even_for_absence() {
        let range = crate::selection::TextRange {
            location: 100,
            length: 3,
        };
        let mut entry = AttributedEntry {
            source: "abc".into(),
            range: Some(range),
            input: None,
        };
        assert!(entry.input_for("abc", Some(range)).unwrap().is_none());
        assert!(entry.input_for("abC", Some(range)).is_err());
        assert!(entry.input_for("abc", None).is_err());
        assert!(
            entry
                .input_for(
                    "abc",
                    Some(crate::selection::TextRange {
                        location: 200,
                        ..range
                    })
                )
                .is_err()
        );
        entry.input = Some(super::super::render::AttributedInput {
            source: "abc".into(),
            runs: vec![],
        });
        assert_eq!(
            entry.input_for("abc", Some(range)).unwrap().unwrap().source,
            "abc"
        );
    }

    #[test]
    fn actual_appkit_list_keys_and_coretext_traits() {
        // These are public framework constants, not guessed accessibility names.
        assert_eq!(
            unsafe { NSAccessibilityLinkTextAttribute }.to_string(),
            "AXLink"
        );
        assert_eq!(
            unsafe { NSAccessibilityAttachmentTextAttribute }.to_string(),
            "AXAttachment"
        );
        assert_eq!(
            unsafe { NSAccessibilityListItemLevelTextAttribute }.to_string(),
            "AXListItemLevel"
        );
        assert_eq!(
            unsafe { NSAccessibilityListItemPrefixTextAttribute }.to_string(),
            "AXListItemPrefix"
        );
        assert_eq!(
            unsafe { NSAccessibilityListItemIndexTextAttribute }.to_string(),
            "AXListItemIndex"
        );
        let font = |name: &str| {
            CFDictionary::from_CFType_pairs(&[
                (CFString::new("AXFontName"), CFString::new(name).as_CFType()),
                (
                    CFString::new("AXFontSize"),
                    CFNumber::from(12.0).as_CFType(),
                ),
            ])
        };
        assert_eq!(font_traits(&font("Helvetica")) & 3, 0);
        assert_eq!(font_traits(&font("Helvetica-Bold")) & 3, 2);
        assert_eq!(font_traits(&font("Helvetica-Oblique")) & 3, 1);
        // A missing size does not turn a font name into a bold predicate.
        assert_eq!(
            font_traits(&CFDictionary::from_CFType_pairs(&[(
                CFString::new("AXFontName"),
                CFString::new("Helvetica-Bold").as_CFType()
            ),])),
            0
        );
    }
}
