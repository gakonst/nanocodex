//! Optional ICU decoder. The selected library and immutable mapping prototype
//! stay pinned for process lifetime; each runtime owns its mutable stream clones.
//! The output capacity and flush/reset ordering follow Node's ICU decoder contract.
use std::{
    ffi::{CStr, CString, c_char, c_void},
    path::PathBuf,
    ptr::NonNull,
    sync::{Arc, OnceLock},
};
#[path = "runtime_icu_data/mod.rs"]
mod data;
type Open = unsafe extern "C" fn(*const c_char, *mut i32) -> *mut c_void;
type OpenPackage = unsafe extern "C" fn(*const c_char, *const c_char, *mut i32) -> *mut c_void;
type CloneConverter = unsafe extern "C" fn(*const c_void, *mut i32) -> *mut c_void;
type SetAppData = unsafe extern "C" fn(*const c_char, *const c_void, *mut i32);
type GetDataDirectory = unsafe extern "C" fn() -> *const c_char;
type Close = unsafe extern "C" fn(*mut c_void);
type Count = unsafe extern "C" fn(*const c_void, *mut i32) -> i32;
type Min = unsafe extern "C" fn(*const c_void) -> i8;
type Convert = unsafe extern "C" fn(
    *mut c_void,
    *mut *mut u16,
    *const u16,
    *mut *const c_char,
    *const c_char,
    *mut i32,
    i8,
    *mut i32,
);
type Callback = unsafe extern "C" fn(*const c_void, *mut c_void, *const c_char, i32, i32, *mut i32);
type SetSubstitution = unsafe extern "C" fn(*mut c_void, *const c_char, i8, *mut i32);
type SetCallback = unsafe extern "C" fn(
    *mut c_void,
    Callback,
    *const c_void,
    *mut Callback,
    *mut *const c_void,
    *mut i32,
);
struct Primary {
    handle: NonNull<c_void>,
    clone_converter: CloneConverter,
    close: Close,
}
// ICU documents ucnv_clone as thread safe. The primary is never decoded,
// reconfigured, or reset after publication; all mutation happens on owned clones.
unsafe impl Send for Primary {}
unsafe impl Sync for Primary {}
impl Drop for Primary {
    fn drop(&mut self) {
        unsafe { (self.close)(self.handle.as_ptr()) };
    }
}
struct PrivateMapping {
    primary: Option<Primary>,
    reason: Option<String>,
}
impl PrivateMapping {
    fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            primary: None,
            reason: Some(reason.into()),
        }
    }
    fn load(library: &libloading::Library, suffix: &str, close: Close) -> Self {
        if cfg!(target_endian = "big") {
            return Self::unavailable("The bundled converter package requires little-endian data");
        }
        let api = unsafe {
            (|| {
                Some((
                    symbol::<OpenPackage>(library, "ucnv_openPackage", suffix)?,
                    symbol::<CloneConverter>(library, "ucnv_clone", suffix)?,
                    symbol::<SetAppData>(library, "udata_setAppData", suffix)?,
                    symbol::<GetDataDirectory>(library, "u_getDataDirectory", suffix)?,
                ))
            })()
        };
        let Some((open_package, clone_converter, set_app_data, get_directory)) = api else {
            return Self::unavailable(
                "ICU application-data or thread-safe clone API is unavailable",
            );
        };
        let directory = unsafe { get_directory() };
        if directory.is_null() || !unsafe { CStr::from_ptr(directory) }.to_bytes().is_empty() {
            // ICU checks individual files before registered packages when this
            // global directory is nonempty. Preserve configured-host behavior,
            // but never claim that such an override uses our pinned mapping.
            return Self::unavailable(
                "Initial ICU data directory is nonempty; preserving configured system converter behavior",
            );
        }
        let mut status = 0;
        unsafe {
            set_app_data(
                data::NAME.as_ptr(),
                data::bytes().as_ptr().cast(),
                &mut status,
            )
        };
        if status != 0 {
            // A duplicate registration can belong to a different embedding
            // caller. Do not assume that its package is our immutable bytes.
            return Self::unavailable(format!(
                "Private package registration returned ICU status {status}"
            ));
        }
        let handle =
            unsafe { open_package(data::NAME.as_ptr(), data::CONVERTER.as_ptr(), &mut status) };
        let Some(handle) = NonNull::new(handle) else {
            return Self::unavailable(format!(
                "Private converter initialization returned ICU status {status}"
            ));
        };
        let primary = Primary {
            handle,
            clone_converter,
            close,
        };
        if status > 0 {
            return Self::unavailable(format!(
                "Private converter initialization returned ICU status {status}"
            ));
        }
        Self {
            primary: Some(primary),
            reason: None,
        }
    }
}
unsafe fn symbol<T: Copy>(library: &libloading::Library, name: &str, suffix: &str) -> Option<T> {
    unsafe {
        library
            .get::<T>(format!("{name}{suffix}\0").as_bytes())
            .ok()
            .map(|s| *s)
    }
}
pub struct Library {
    open: Open,
    close: Close,
    count: Count,
    min: Min,
    convert: Convert,
    set_callback: SetCallback,
    stop: Callback,
    set_substitution: SetSubstitution,
    pub version: String,
    pub path: String,
    // Field order ensures that a primary closes before its library, including
    // any initialization failure before the successful process pin is installed.
    private_mapping: PrivateMapping,
    _library: libloading::Library,
}
impl Library {
    pub fn load() -> Option<Arc<Self>> {
        static LIBRARY: OnceLock<Option<Arc<Library>>> = OnceLock::new();
        LIBRARY.get_or_init(Self::load_once).clone()
    }
    fn load_once() -> Option<Arc<Self>> {
        let mut paths = Vec::new();
        #[cfg(target_os = "macos")]
        {
            for base in ["/opt/homebrew/opt", "/usr/local/opt"] {
                for version in (60..=90).rev() {
                    paths.push(PathBuf::from(format!(
                        "{base}/icu4c@{version}/lib/libicuuc.dylib"
                    )));
                }
            }
            paths.push(PathBuf::from("/usr/lib/libicucore.A.dylib"));
        }
        #[cfg(target_os = "linux")]
        for base in [
            "/usr/lib/x86_64-linux-gnu",
            "/usr/lib/aarch64-linux-gnu",
            "/usr/lib64",
            "/usr/lib",
        ] {
            for version in (60..=90).rev() {
                paths.push(PathBuf::from(format!("{base}/libicuuc.so.{version}")));
            }
        }
        #[cfg(target_os = "windows")]
        if let Some(root) = std::env::var_os("SystemRoot") {
            let root = PathBuf::from(root);
            if root.is_absolute() {
                paths.push(root.join("System32/icu.dll"));
            }
        }
        for path in paths {
            // Absolute platform installation paths only; never search cwd or page data.
            let library = match unsafe { libloading::Library::new(&path) } {
                Ok(lib) => lib,
                Err(_) => continue,
            };
            for suffix in
                std::iter::once(String::new()).chain((60..=90).rev().map(|v| format!("_{v}")))
            {
                let loaded = (|| unsafe {
                    let version_fn: unsafe extern "C" fn(*mut u8) =
                        symbol(&library, "u_getVersion", &suffix)?;
                    let mut version = [0u8; 4];
                    version_fn(version.as_mut_ptr());
                    Some((
                        symbol(&library, "ucnv_open", &suffix)?,
                        symbol(&library, "ucnv_close", &suffix)?,
                        symbol(&library, "ucnv_toUCountPending", &suffix)?,
                        symbol(&library, "ucnv_getMinCharSize", &suffix)?,
                        symbol(&library, "ucnv_toUnicode", &suffix)?,
                        symbol(&library, "ucnv_setToUCallBack", &suffix)?,
                        symbol(&library, "UCNV_TO_U_CALLBACK_STOP", &suffix)?,
                        symbol(&library, "ucnv_setSubstChars", &suffix)?,
                        version,
                    ))
                })();
                if let Some((
                    open,
                    close,
                    count,
                    min,
                    convert,
                    set_callback,
                    stop,
                    set_substitution,
                    version,
                )) = loaded
                {
                    let private_mapping = PrivateMapping::load(&library, &suffix, close);
                    return Some(Arc::new(Self {
                        open,
                        close,
                        count,
                        min,
                        convert,
                        set_callback,
                        stop,
                        set_substitution,
                        version: version
                            .iter()
                            .map(u8::to_string)
                            .collect::<Vec<_>>()
                            .join("."),
                        path: path.to_string_lossy().into_owned(),
                        private_mapping,
                        _library: library,
                    }));
                }
            }
        }
        None
    }
    pub fn private_mapping_status(&self) -> serde_json::Value {
        serde_json::json!({
            "active": self.private_mapping.primary.is_some(),
            "reason": self.private_mapping.reason,
            "sourceIcuVersion": "78.3",
            "sourceCommit": data::SOURCE_COMMIT,
            "sourceConverter": "gb18030-2022",
            "compilerIcuVersion": "72.1",
            "converterFormatVersion": "6.2",
            "packageFormatVersion": "1.0",
            "packageSha256": data::SHA256,
            "packageBytes": data::bytes().len(),
            "libraryLifetime": "process",
            "converterOwnership": "independent clones of immutable primary",
        })
    }
    pub fn decoder(
        self: &Arc<Self>,
        encoding: &str,
        fatal: bool,
        ignore_bom: bool,
    ) -> Option<Decoder> {
        let name = CString::new(if encoding == "gbk" {
            "gb18030"
        } else {
            encoding
        })
        .ok()?;
        let mut status = 0;
        let handle = match (&self.private_mapping.primary, encoding) {
            (Some(primary), "gbk" | "gb18030") => unsafe {
                (primary.clone_converter)(primary.handle.as_ptr(), &mut status)
            },
            _ => unsafe { (self.open)(name.as_ptr(), &mut status) },
        };
        if handle.is_null() {
            return None;
        }
        let mut decoder = Decoder {
            library: self.clone(),
            handle,
            ignore_bom,
            bom_seen: false,
            unicode: encoding.starts_with("utf-16"),
            min: 1,
        };
        if status > 0 {
            return None;
        }
        if fatal {
            unsafe {
                (self.set_callback)(
                    handle,
                    self.stop,
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &mut status,
                )
            };
            if status > 0 {
                return None;
            }
        }
        decoder.min = usize::try_from(unsafe { (self.min)(handle) })
            .ok()
            .filter(|n| (1..=8).contains(n))?;
        let substitution = vec![b'?' as c_char; decoder.min];
        unsafe {
            (self.set_substitution)(
                handle,
                substitution.as_ptr(),
                decoder.min as i8,
                &mut status,
            )
        };
        if status > 0 {
            return None;
        }
        Some(decoder)
    }
}
pub struct Decoder {
    library: Arc<Library>,
    handle: *mut c_void,
    min: usize,
    ignore_bom: bool,
    bom_seen: bool,
    unicode: bool,
}
impl Decoder {
    pub fn decode(&mut self, input: &[u8], last: bool) -> std::io::Result<(String, bool)> {
        let mut status = 0;
        let pending = if last {
            let n = unsafe { (self.library.count)(self.handle, &mut status) };
            if status > 0 || !(0..=32).contains(&n) {
                return Err(std::io::Error::other(
                    "ICU returned invalid pending-byte count",
                ));
            }
            n as usize
        } else {
            0
        };
        let capacity = 2 * self.min * input.len().max(pending);
        if capacity > 16 * 1024 * 1024 {
            return Err(std::io::Error::other(
                "ICU decode allocation exceeds 32 MiB",
            ));
        }
        let mut output = vec![0u16; capacity.max(1)];
        let mut destination = output.as_mut_ptr();
        let mut source = input.as_ptr().cast::<c_char>();
        unsafe {
            (self.library.convert)(
                self.handle,
                &mut destination,
                output.as_ptr().add(capacity),
                &mut source,
                source.add(input.len()),
                std::ptr::null_mut(),
                i8::from(last),
                &mut status,
            )
        };
        if status > 0 {
            return Ok((String::new(), true));
        }
        let length = (destination as usize)
            .checked_sub(output.as_ptr() as usize)
            .filter(|n| *n % 2 == 0 && *n / 2 <= capacity)
            .ok_or_else(|| std::io::Error::other("ICU returned invalid output pointer"))?
            / 2;
        let mut start = 0;
        if length > 0 && self.unicode && !self.ignore_bom && !self.bom_seen {
            self.bom_seen = true;
            if output[0] == 0xfeff {
                start = 1;
            }
        }
        let text = String::from_utf16(&output[start..length])
            .map_err(|_| std::io::Error::other("ICU returned invalid UTF-16"))?;
        if text.len() > 8 * 1024 * 1024 {
            return Err(std::io::Error::other("Decoded text exceeds 8 MiB"));
        }
        Ok((text, false))
    }
}
impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe { (self.library.close)(self.handle) };
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};

    fn oracle() -> Value {
        serde_json::from_str(include_str!(
            "../tests/oracles/runtime_icu_data_2026-09-07.json"
        ))
        .unwrap()
    }

    #[test]
    fn pinned_icu_converter_matches_all_installed_single_and_double_byte_results() {
        let library = Library::load().expect("ICU required for converter conformance");
        assert!(
            library.private_mapping.primary.is_some(),
            "{}",
            library.private_mapping_status()
        );
        let oracle = oracle();
        for label in ["gbk", "gb18030"] {
            for fatal in [false, true] {
                let mut results = Vec::with_capacity(65792);
                for index in 0..65792 {
                    let input = if index < 256 {
                        vec![index as u8]
                    } else {
                        let value = index - 256;
                        vec![(value >> 8) as u8, value as u8]
                    };
                    let (text, invalid) = library
                        .decoder(label, fatal, false)
                        .unwrap()
                        .decode(&input, true)
                        .unwrap();
                    results.push(json!([invalid, text]));
                }
                let hash = format!(
                    "{:x}",
                    Sha256::digest(serde_json::to_vec(&results).unwrap())
                );
                assert_eq!(
                    hash,
                    oracle["tableHashes"][fatal.to_string()],
                    "{label}/{fatal}"
                );
            }
        }
    }

    #[test]
    fn pinned_icu_stream_clones_match_installed_partitions_fatal_flush_and_reuse() {
        let library = Library::load().expect("ICU required for stream conformance");
        for label in ["gbk", "gb18030"] {
            for (index, case) in oracle()["streams"].as_array().unwrap().iter().enumerate() {
                let fatal = case["fatal"].as_bool().unwrap();
                let mut decoder = library.decoder(label, fatal, false).unwrap();
                let mut output = Vec::new();
                for call in case["calls"].as_array().unwrap() {
                    let bytes: Vec<u8> = call[0]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|x| x.as_u64().unwrap() as u8)
                        .collect();
                    let stream = call[1].as_bool().unwrap();
                    let (text, invalid) = decoder.decode(&bytes, !stream).unwrap();
                    output.push(json!([invalid, text]));
                    // Codecs removes and drops its owned native stream on every
                    // non-streaming decode, including fatal flush errors.
                    if !stream {
                        decoder = library.decoder(label, fatal, false).unwrap();
                    }
                }
                assert_eq!(Value::Array(output), case["out"], "{label}/stream {index}");
            }
        }
    }
}
