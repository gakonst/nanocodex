//! Native C bindings to the same protocol core exported through wasm-bindgen.
use nanocodex_voice_protocol::ManagedVoiceProtocol;
use serde_json::json;
use std::{
    collections::HashMap,
    ffi::{CString, c_char},
    sync::{LazyLock, Mutex},
};

#[derive(Default)]
struct Voices {
    next: u64,
    values: HashMap<u64, ManagedVoiceProtocol>,
}
static VOICES: LazyLock<Mutex<Voices>> = LazyLock::new(|| Mutex::new(Voices::default()));

/// The caller owns an immutable buffer valid for this call. No caller
/// memory is retained by the Rust protocol after returning.
unsafe fn input<'a>(bytes: *const u8, length: usize) -> Option<&'a str> {
    if bytes.is_null() || length > 256 * 1024 {
        return None;
    }
    // SAFETY: the C ABI requires `length` readable bytes for the call's duration.
    std::str::from_utf8(unsafe { std::slice::from_raw_parts(bytes, length) }).ok()
}

/// Creates one owned protocol handle, or zero for invalid input/capacity.
///
/// # Safety
/// `bytes` must point to `length` readable bytes for the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nc_voice_create(bytes: *const u8, length: usize) -> u64 {
    // SAFETY: forwarded from the C caller's buffer contract.
    let Some(voice) = (unsafe { input(bytes, length) }) else {
        return 0;
    };
    let Ok(protocol) = ManagedVoiceProtocol::new(voice) else {
        return 0;
    };
    let Ok(mut voices) = VOICES.lock() else {
        return 0;
    };
    if voices.values.len() >= 128 {
        return 0;
    }
    let Some(id) = voices.next.checked_add(1) else {
        return 0;
    };
    voices.next = id;
    voices.values.insert(id, protocol);
    id
}

/// Applies a JSON command and returns an owned JSON result. Release the returned
/// string exactly once with `nc_voice_string_free`.
///
/// # Safety
/// `bytes` must point to `length` readable bytes for the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nc_voice_apply(
    handle: u64,
    bytes: *const u8,
    length: usize,
) -> *mut c_char {
    let result = std::panic::catch_unwind(|| {
        // SAFETY: forwarded from the C caller's buffer contract.
        let text = unsafe { input(bytes, length) }.ok_or("invalid voice input".to_owned())?;
        let command = serde_json::from_str(text).map_err(|_| "invalid voice JSON".to_owned())?;
        let mut voices = VOICES.lock().map_err(|_| "voice unavailable".to_owned())?;
        let protocol = voices
            .values
            .get_mut(&handle)
            .ok_or("voice is closed".to_owned())?;
        protocol.dispatch(&command)
    })
    .unwrap_or_else(|_| Err("voice protocol failed".to_owned()));
    let encoded = match result {
        Ok(value) => json!({"value": value}),
        Err(error) => json!({"error": error}),
    }
    .to_string();
    // JSON encoding escapes NUL bytes. Return null if that invariant changes.
    CString::new(encoded).map_or(std::ptr::null_mut(), CString::into_raw)
}

/// Releases an owned handle. Releasing a stale handle is harmless.
#[unsafe(no_mangle)]
pub extern "C" fn nc_voice_destroy(handle: u64) {
    if let Ok(mut voices) = VOICES.lock() {
        voices.values.remove(&handle);
    }
}

/// Releases a result string.
///
/// # Safety
/// `value` must be null or an unfreed pointer returned by `nc_voice_apply`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nc_voice_string_free(value: *mut c_char) {
    if !value.is_null() {
        // SAFETY: only the caller that owns this returned allocation may free it.
        drop(unsafe { CString::from_raw(value) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CStr;
    fn apply(handle: u64, text: &str) -> serde_json::Value {
        // SAFETY: borrowed input lives through the call; returned allocation is
        // copied then freed exactly once using its matching Rust allocator.
        unsafe {
            let ptr = nc_voice_apply(handle, text.as_ptr(), text.len());
            assert!(!ptr.is_null());
            let result = serde_json::from_str(CStr::from_ptr(ptr).to_str().unwrap()).unwrap();
            nc_voice_string_free(ptr);
            result
        }
    }
    #[test]
    fn native_abi_owns_handles_and_uses_the_same_bootstrap_reducer() {
        // SAFETY: both literal buffers are valid for the call's duration.
        let handle = unsafe { nc_voice_create(b"cove".as_ptr(), 4) };
        assert_ne!(handle, 0);
        apply(handle, r#"{"op":"bind","session_id":"call-1"}"#);
        assert_eq!(
            apply(handle, r#"{"op":"opened"}"#)["value"]["playback_enabled"],
            false
        );
        let result = apply(
            handle,
            r#"{"op":"realtime","event":{"type":"turn.done","turn":{"role":"user","transcript":"Elena's birthday?"}}}"#,
        );
        assert!(
            result["value"]["delegation"]["formatted_input"]
                .as_str()
                .unwrap()
                .contains("voice_bootstrap")
        );
        assert!(apply(handle, "bad json")["error"].is_string());
        nc_voice_destroy(handle);
        nc_voice_destroy(handle);
        assert_eq!(
            apply(handle, r#"{"op":"opened"}"#)["error"],
            "voice is closed"
        );
        assert_eq!(unsafe { nc_voice_create(b"invalid".as_ptr(), 7) }, 0);
    }
}
