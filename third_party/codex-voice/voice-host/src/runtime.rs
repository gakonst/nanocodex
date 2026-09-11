//! Initialize only the physical package's native runtime on the helper worker.
//!
//! GStreamer registers process-global callbacks. Keep every opened library loaded
//! through process exit, including partial initialization failures. This private
//! bootstrap is not a media binding API and never opens an audio device.

use std::ffi::CString;
use std::ffi::c_char;
use std::ffi::c_void;
use std::io;
use std::mem::ManuallyDrop;
use std::path::Path;
use std::path::PathBuf;
use std::ptr;

use codex_realtime_webrtc::RUNTIME_ENVIRONMENT;
use libloading::Library;

type Init = unsafe extern "C" fn(*mut i32, *mut *mut *mut c_char, *mut *mut c_void) -> i32;
type LoadPlugin = unsafe extern "C" fn(*const c_char, *mut *mut c_void) -> *mut c_void;
type Unref = unsafe extern "C" fn(*mut c_void);

pub(super) struct Runtime {
    _libraries: Vec<ManuallyDrop<Library>>,
}

impl Runtime {
    pub(super) fn initialize() -> io::Result<Self> {
        if RUNTIME_ENVIRONMENT.iter().any(|(key, value)| {
            std::env::var_os(key).as_deref() != Some(std::ffi::OsStr::new(value))
        }) {
            return Err(io::Error::other("private runtime environment is required"));
        }
        let executable = std::env::current_exe()?.canonicalize()?;
        let bin = executable.parent().ok_or_else(runtime_error)?;
        let root = bin.parent().ok_or_else(runtime_error)?;
        if bin.file_name().is_none_or(|name| name != "bin")
            || root.file_name().is_none_or(|name| name != "voice")
            || root
                .parent()
                .and_then(Path::file_name)
                .is_none_or(|name| name != "nanocodex-resources")
        {
            return Err(runtime_error());
        }
        let (core, directory, prefix, suffix) = if cfg!(target_os = "macos") {
            (
                "lib/libgstreamer-1.0.0.dylib",
                "plugins",
                "libgst",
                ".dylib",
            )
        } else if cfg!(all(target_os = "linux", target_env = "gnu")) {
            (
                "lib/libgstreamer-1.0.so.0",
                "lib/gstreamer-1.0",
                "libgst",
                ".so",
            )
        } else if cfg!(all(windows, target_env = "msvc")) {
            ("bin/gstreamer-1.0-0.dll", "bin", "gst", ".dll")
        } else {
            return Err(io::Error::other("unsupported native runtime platform"));
        };
        let core = private_file(root, core)?;
        let plugins = [
            "app",
            "audioconvert",
            "audioresample",
            "coreelements",
            "opus",
            "rtp",
            "rtpmanager",
        ]
        .map(|name| private_file(root, &format!("{directory}/{prefix}{name}{suffix}")))
        .into_iter()
        .collect::<io::Result<Vec<_>>>()?;
        let library = load(&core)?;
        // SAFETY: These are stable GStreamer 1.x C signatures. The library stays
        // loaded through process exit, and all calls occur on this one worker.
        let (init, load_plugin, unref) = unsafe {
            (
                *library
                    .get::<Init>(b"gst_init_check\0")
                    .map_err(|_| runtime_error())?,
                *library
                    .get::<LoadPlugin>(b"gst_plugin_load_file\0")
                    .map_err(|_| runtime_error())?,
                *library
                    .get::<Unref>(b"gst_object_unref\0")
                    .map_err(|_| runtime_error())?,
            )
        };
        let mut libraries = vec![library];
        // SAFETY: GStreamer accepts null argc/argv and an omitted error output.
        // Fixed child settings disable registry reads, scans, writes and forking.
        if unsafe { init(ptr::null_mut(), ptr::null_mut(), ptr::null_mut()) } == 0 {
            return Err(runtime_error());
        }
        for plugin in plugins {
            // Preload with the restricted OS search policy before GStreamer
            // opens the same module, including its private dependencies on Windows.
            libraries.push(load(&plugin)?);
            #[cfg(unix)]
            let filename = {
                use std::os::unix::ffi::OsStrExt;
                CString::new(plugin.as_os_str().as_bytes())
            };
            #[cfg(windows)]
            let filename = CString::new(plugin.to_str().ok_or_else(runtime_error)?);
            let filename = filename.map_err(|_| runtime_error())?;
            // SAFETY: The NUL-terminated filename lives through the call. A
            // non-null result owns a plugin reference, released exactly once.
            unsafe {
                let plugin = load_plugin(filename.as_ptr(), ptr::null_mut());
                if plugin.is_null() {
                    return Err(runtime_error());
                }
                unref(plugin);
            }
        }
        Ok(Self {
            _libraries: libraries,
        })
    }
}

fn private_file(root: &Path, relative: &str) -> io::Result<PathBuf> {
    let path = root.join(relative);
    if path.canonicalize()? != path || !path.is_file() {
        return Err(runtime_error());
    }
    Ok(path)
}

fn load(path: &Path) -> io::Result<ManuallyDrop<Library>> {
    // SAFETY: Callers supply only absolute, physical paths inside the installed
    // trusted package. Native code runs only in this expendable helper process.
    #[cfg(unix)]
    let library = unsafe {
        libloading::os::unix::Library::open(
            Some(path),
            libloading::os::unix::RTLD_NOW | libloading::os::unix::RTLD_LOCAL,
        )
        .map(Library::from)
    };
    #[cfg(windows)]
    let library = unsafe {
        libloading::os::windows::Library::load_with_flags(
            path,
            libloading::os::windows::LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR
                | libloading::os::windows::LOAD_LIBRARY_SEARCH_SYSTEM32,
        )
        .map(Library::from)
    };
    library.map(ManuallyDrop::new).map_err(|_| runtime_error())
}

fn runtime_error() -> io::Error {
    io::Error::other("private audio runtime initialization failed")
}
