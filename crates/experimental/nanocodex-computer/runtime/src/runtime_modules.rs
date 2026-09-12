//! Owned filesystem and codec operations for the embedded builtin modules.
use base64::Engine as _;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};
const LIMIT: usize = 8 * 1024 * 1024;
pub fn canonical_module(name: &str) -> Option<String> {
    if matches!(name, "skyre:kernel" | "@oai/cua/tinyskyAlt") {
        return Some(name.into());
    }
    let name = name.strip_prefix("node:").unwrap_or(name);
    [
        "fs/promises",
        "path",
        "path/posix",
        "path/win32",
        "buffer",
        "url",
        "timers",
        "timers/promises",
        "string_decoder",
        "querystring",
        "events",
    ]
    .contains(&name)
    .then(|| format!("node:{name}"))
}
pub struct Resolver;
impl rquickjs::loader::Resolver for Resolver {
    fn resolve<'js>(
        &mut self,
        ctx: &rquickjs::Ctx<'js>,
        base: &str,
        name: &str,
        _: Option<rquickjs::loader::ImportAttributes<'js>>,
    ) -> rquickjs::Result<String> {
        if matches!(name, "process" | "node:process") {
            return Err(rquickjs::Exception::throw_message(
                ctx,
                &format!("Importing module \"{name}\" is not allowed in node_repl"),
            ));
        }
        canonical_module(name).ok_or_else(|| rquickjs::Error::new_resolving(base, name))
    }
}
struct ModuleSources(Vec<(&'static str, String)>);
impl ModuleSources {
    fn with_module(mut self, name: &'static str, source: impl Into<String>) -> Self {
        self.0.push((name, source.into()));
        self
    }
}
pub fn builtin_sources() -> Vec<(&'static str, String)> {
    let exports = |global: &str, names: &[&str]| {
        format!(
            "const m=globalThis.__skyreModules.{global};export default m;{}",
            names
                .iter()
                .map(|name| format!("export const {name}=m.{name};"))
                .collect::<String>()
        )
    };
    let paths = [
        "resolve",
        "normalize",
        "isAbsolute",
        "join",
        "relative",
        "toNamespacedPath",
        "dirname",
        "basename",
        "extname",
        "format",
        "parse",
        "sep",
        "delimiter",
        "win32",
        "posix",
    ];
    let timer_exports = |field: &str, names: &[&str]| {
        format!(
            "const m=globalThis.__skyreTimers.{field};export default m;{}",
            names
                .iter()
                .map(|name| format!("export const {name}=m.{name};"))
                .collect::<String>()
        )
    };
    ModuleSources(vec![])
        .with_module("@oai/cua/tinyskyAlt", include_str!("runtime_cua_setup.js"))
        .with_module("node:events", include_str!("runtime_events.js"))
        .with_module("node:querystring", include_str!("runtime_querystring.js"))
        .with_module(
            "node:string_decoder",
            include_str!("runtime_string_decoder.js"),
        )
        .with_module(
            "node:timers",
            timer_exports(
                "timers",
                &[
                    "setTimeout",
                    "clearTimeout",
                    "setImmediate",
                    "clearImmediate",
                    "setInterval",
                    "clearInterval",
                    "promises",
                ],
            ),
        )
        .with_module(
            "node:timers/promises",
            timer_exports(
                "promises",
                &["setTimeout", "setImmediate", "setInterval", "scheduler"],
            ),
        )
        .with_module(
            "node:url",
            exports(
                "url",
                &[
                    "URL",
                    "URLSearchParams",
                    "fileURLToPath",
                    "fileURLToPathBuffer",
                    "pathToFileURL",
                    "domainToASCII",
                    "domainToUnicode",
                    "format",
                    "urlToHttpOptions",
                ],
            ),
        )
        .with_module("node:path", exports("path", &paths))
        .with_module("node:path/posix", exports("path.posix", &paths))
        .with_module("node:path/win32", exports("path.win32", &paths))
        .with_module(
            "node:fs/promises",
            exports(
                "fs",
                &[
                    "constants",
                    "readFile",
                    "writeFile",
                    "appendFile",
                    "open",
                    "mkdir",
                    "mkdtemp",
                    "readdir",
                    "stat",
                    "lstat",
                    "access",
                    "realpath",
                    "readlink",
                    "unlink",
                    "rm",
                    "rmdir",
                    "rename",
                    "copyFile",
                    "link",
                    "symlink",
                    "truncate",
                    "chmod",
                ],
            ),
        )
        .with_module(
            "node:buffer",
            "export const Buffer=globalThis.__skyreModules.Buffer;export default {Buffer};",
        )
        .0
}
pub fn loader() -> rquickjs::loader::BuiltinLoader {
    builtin_sources().into_iter().fold(
        rquickjs::loader::BuiltinLoader::default(),
        |loader, (name, source)| loader.with_module(name, source),
    )
}
#[derive(Default)]
pub struct Files {
    handles: BTreeMap<u32, File>,
    next: u32,
}
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
fn mode(value: Option<&Value>, default: u32) -> io::Result<u32> {
    match value {
        None | Some(Value::Null) => Ok(default),
        Some(Value::String(text)) => {
            u32::from_str_radix(text, 8).map_err(|_| invalid("Mode must be an octal string"))
        }
        Some(value) => value
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| invalid("Mode must be a positive integer")),
    }
}
fn mode_from_args(args: &Value, default: u32) -> io::Result<u32> {
    mode(args.get("mode"), default)
}
fn same_file(input: &File, destination: &Path) -> io::Result<bool> {
    let target = match fs::metadata(destination) {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    let source = input.metadata()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(source.dev() == target.dev() && source.ino() == target.ino())
    }
    #[cfg(not(unix))]
    {
        let _ = (source, target);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Overwrite copy requires file identity support on this target",
        ))
    }
}
fn path(value: &Value) -> io::Result<PathBuf> {
    if let Some(url) = value["url"].as_str() {
        if url.len() > 128 * 1024 {
            return Err(invalid("Path exceeds 128 KiB"));
        }
        return url::Url::parse(url)
            .map_err(|_| invalid("Invalid file URL"))?
            .to_file_path()
            .map_err(|_| invalid("Expected a local file URL"));
    }
    let path = value
        .as_str()
        .ok_or_else(|| invalid("Path must be a string, Buffer or file URL"))?;
    if path.len() > 128 * 1024 {
        return Err(invalid("Path exceeds 128 KiB"));
    }
    Ok(path.into())
}
fn bytes(value: &Value) -> io::Result<Vec<u8>> {
    let encoded = value
        .as_str()
        .ok_or_else(|| invalid("Expected encoded bytes"))?;
    if encoded.len() > LIMIT * 4 / 3 + 16 {
        return Err(invalid("Filesystem data exceeds 8 MiB"));
    }
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| invalid("Invalid bytes"))
}
fn data(bytes: &[u8]) -> Value {
    json!({"data":base64::engine::general_purpose::STANDARD.encode(bytes)})
}
fn read_bounded(file: &mut File) -> io::Result<Value> {
    if !file.metadata()?.is_file() {
        return Err(invalid("readFile requires a regular file"));
    }
    let mut output = vec![];
    file.take(LIMIT as u64 + 1).read_to_end(&mut output)?;
    if output.len() > LIMIT {
        return Err(invalid("Filesystem data exceeds 8 MiB"));
    }
    Ok(data(&output))
}
fn open(path: &Path, flag: &str, mode: u32) -> io::Result<File> {
    let mut options = OpenOptions::new();
    match flag {
        "r" => {
            options.read(true);
        }
        "r+" => {
            options.read(true).write(true);
        }
        "w" | "w+" => {
            options
                .write(true)
                .read(flag.ends_with('+'))
                .create(true)
                .truncate(true);
        }
        "wx" | "wx+" | "xw" | "xw+" => {
            options
                .write(true)
                .read(flag.ends_with('+'))
                .create_new(true);
        }
        "a" | "a+" => {
            options.append(true).read(flag.ends_with('+')).create(true);
        }
        "ax" | "ax+" | "xa" | "xa+" => {
            options
                .append(true)
                .read(flag.ends_with('+'))
                .create_new(true);
        }
        _ => return Err(invalid("Unsupported file flag")),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(mode).custom_flags(libc::O_NONBLOCK);
    }
    options.open(path)
}
fn millis(time: io::Result<std::time::SystemTime>) -> f64 {
    time.ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0.0, |duration| duration.as_secs_f64() * 1000.0)
}
fn metadata(meta: fs::Metadata) -> Value {
    let mut out = json!({"size":meta.len(),"atimeMs":millis(meta.accessed()),"mtimeMs":millis(meta.modified()),"ctimeMs":millis(meta.modified()),"birthtimeMs":millis(meta.created()),"isFile":meta.is_file(),"isDirectory":meta.is_dir(),"isSymbolicLink":meta.file_type().is_symlink(),"isBlockDevice":false,"isCharacterDevice":false,"isFIFO":false,"isSocket":false,"dev":0,"ino":0,"mode":0,"nlink":1,"uid":0,"gid":0,"rdev":0,"blksize":0,"blocks":0});
    #[cfg(unix)]
    {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        for (name, value) in [
            ("dev", meta.dev()),
            ("ino", meta.ino()),
            ("mode", meta.mode() as u64),
            ("nlink", meta.nlink()),
            ("uid", meta.uid() as u64),
            ("gid", meta.gid() as u64),
            ("rdev", meta.rdev()),
            ("blksize", meta.blksize()),
            ("blocks", meta.blocks()),
        ] {
            out[name] = json!(value)
        }
        out["ctimeMs"] = json!(meta.ctime() as f64 * 1000.0 + meta.ctime_nsec() as f64 / 1e6);
        let file_type = meta.file_type();
        out["isBlockDevice"] = json!(file_type.is_block_device());
        out["isCharacterDevice"] = json!(file_type.is_char_device());
        out["isFIFO"] = json!(file_type.is_fifo());
        out["isSocket"] = json!(file_type.is_socket());
    }
    out
}
impl Files {
    pub fn call(&mut self, request: &str) -> String {
        let value:Value=match serde_json::from_str(request){Ok(value)=>value,Err(_)=>return json!({"error":{"code":"ERR_INVALID_ARG_TYPE","message":"Invalid filesystem request"}}).to_string()};
        let method = value["method"].as_str().unwrap_or("");
        match self.execute(method,&value){Ok(value)=>json!({"result":value}),Err(error)=>{
            let code=if error.raw_os_error()==Some(libc::EBADF){"EBADF"}else if method=="rm"&&error.kind()==io::ErrorKind::IsADirectory{"ERR_FS_EISDIR"}else{match error.kind(){io::ErrorKind::NotFound=>"ENOENT",io::ErrorKind::PermissionDenied=>"EACCES",io::ErrorKind::AlreadyExists=>"EEXIST",io::ErrorKind::InvalidInput=>"EINVAL",io::ErrorKind::NotADirectory=>"ENOTDIR",io::ErrorKind::IsADirectory=>"EISDIR",io::ErrorKind::DirectoryNotEmpty=>"ENOTEMPTY",_=>"EIO"}};
            json!({"error":{"code":code,"errno":error.raw_os_error().map(|code|-code),"syscall":method,"path":value["path"],"message":format!("{code}: {error}")}})
        }}.to_string()
    }
    fn execute(&mut self, method: &str, args: &Value) -> io::Result<Value> {
        let path_arg = || path(&args["path"]);
        let mode = mode_from_args(args, 0o666)?;
        if method == "close" {
            return self
                .handles
                .remove(
                    &(args["handle"]
                        .as_u64()
                        .ok_or_else(|| invalid("Invalid file handle"))?
                        as u32),
                )
                .map(|_| Value::Null)
                .ok_or_else(|| io::Error::from_raw_os_error(libc::EBADF));
        }
        if let Some(handle) = args["handle"].as_u64() {
            let file = self
                .handles
                .get_mut(&(handle as u32))
                .ok_or_else(|| io::Error::from_raw_os_error(libc::EBADF))?;
            return match method {
                "readFile" => read_bounded(file),
                "writeFile" | "appendFile" => {
                    file.write_all(&bytes(&args["data"])?)?;
                    Ok(Value::Null)
                }
                "stat" => Ok(metadata(file.metadata()?)),
                "truncate" => {
                    file.set_len(args["length"].as_u64().unwrap_or(0))?;
                    Ok(Value::Null)
                }
                "sync" => {
                    file.sync_all()?;
                    Ok(Value::Null)
                }
                "datasync" => {
                    file.sync_data()?;
                    Ok(Value::Null)
                }
                "read" | "write" => {
                    let position = args["position"].as_u64();
                    let saved = if position.is_some() {
                        Some(file.stream_position()?)
                    } else {
                        None
                    };
                    if let Some(position) = position {
                        file.seek(SeekFrom::Start(position))?;
                    }
                    let result = (|| {
                        if method == "read" {
                            let length = args["length"]
                                .as_u64()
                                .ok_or_else(|| invalid("Read length is required"))?
                                as usize;
                            if length > LIMIT {
                                return Err(invalid("Read exceeds 8 MiB"));
                            }
                            let mut output = vec![0; length];
                            let count = file.read(&mut output)?;
                            output.truncate(count);
                            let mut result = data(&output);
                            result["bytesRead"] = json!(count);
                            Ok(result)
                        } else {
                            let count = file.write(&bytes(&args["data"])?)?;
                            Ok(json!({"bytesWritten":count}))
                        }
                    })();
                    if let Some(saved) = saved {
                        file.seek(SeekFrom::Start(saved))?;
                    }
                    result
                }
                _ => Err(invalid("Unsupported FileHandle operation")),
            };
        }
        match method {
            "readFile" => read_bounded(&mut open(
                &path_arg()?,
                args["flag"].as_str().unwrap_or("r"),
                mode,
            )?),
            "writeFile" | "appendFile" => {
                let default = if method == "appendFile" { "a" } else { "w" };
                let mut file = open(&path_arg()?, args["flag"].as_str().unwrap_or(default), mode)?;
                if !file.metadata()?.is_file() {
                    return Err(invalid("writeFile requires a regular file"));
                }
                file.write_all(&bytes(&args["data"])?)?;
                if args["flush"] == true {
                    file.sync_all()?;
                }
                Ok(Value::Null)
            }
            "open" => {
                if self.handles.len() >= 1024 {
                    return Err(invalid("Too many open file handles"));
                }
                let file = open(&path_arg()?, args["flag"].as_str().unwrap_or("r"), mode)?;
                // Published handles must preserve the regular-file boundary
                // used by readFile/writeFile, including positioned operations.
                // Inspect the opened file itself before assigning an ID.
                if !file.metadata()?.is_file() {
                    return Err(invalid("open requires a regular file"));
                }
                self.next = self
                    .next
                    .checked_add(1)
                    .ok_or_else(|| invalid("File handle IDs exhausted"))?;
                self.handles.insert(self.next, file);
                Ok(json!(self.next))
            }
            "stat" => Ok(metadata(fs::metadata(path_arg()?)?)),
            "lstat" => Ok(metadata(fs::symlink_metadata(path_arg()?)?)),
            "realpath" => Ok(json!(fs::canonicalize(path_arg()?)?.to_string_lossy())),
            "readlink" => Ok(json!(fs::read_link(path_arg()?)?.to_string_lossy())),
            "mkdir" => {
                let path = path_arg()?;
                if args["recursive"] == true {
                    let mut first = None;
                    let mut ancestor = path.as_path();
                    while !ancestor.exists() {
                        first = Some(ancestor.to_path_buf());
                        let Some(parent) = ancestor.parent().filter(|p| !p.as_os_str().is_empty())
                        else {
                            break;
                        };
                        ancestor = parent;
                    }
                    let mut builder = fs::DirBuilder::new();
                    builder.recursive(true);
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::DirBuilderExt;
                        builder.mode(mode_from_args(args, 0o777)?);
                    }
                    builder.create(&path)?;
                    Ok(first.map_or(Value::Null, |path| json!(path.to_string_lossy())))
                } else {
                    let mut builder = fs::DirBuilder::new();
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::DirBuilderExt;
                        builder.mode(mode_from_args(args, 0o777)?);
                    }
                    builder.create(path)?;
                    Ok(Value::Null)
                }
            }
            "mkdtemp" => {
                let prefix = path_arg()?;
                for _ in 0..100 {
                    let mut random = [0u8; 6];
                    getrandom::fill(&mut random).map_err(|_| {
                        io::Error::other("Cannot generate temporary directory identity")
                    })?;
                    let suffix = random
                        .iter()
                        .map(|b| {
                            (b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"
                                [(b % 62) as usize]) as char
                        })
                        .collect::<String>();
                    let path = PathBuf::from(format!("{}{suffix}", prefix.to_string_lossy()));
                    let mut builder = fs::DirBuilder::new();
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::DirBuilderExt;
                        builder.mode(0o700);
                    }
                    match builder.create(&path) {
                        Ok(()) => return Ok(json!(path.to_string_lossy())),
                        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (),
                        Err(error) => return Err(error),
                    }
                }
                Err(io::Error::other("Cannot create temporary directory"))
            }
            "readdir" => {
                let parent = path_arg()?;
                let mut output = vec![];
                for entry in fs::read_dir(&parent)? {
                    if output.len() >= 10000 {
                        return Err(invalid("Directory exceeds 10000 entries"));
                    }
                    let entry = entry?;
                    if args["withFileTypes"] == true {
                        let mut value = metadata(fs::symlink_metadata(entry.path())?);
                        value["name"] = json!(entry.file_name().to_string_lossy());
                        value["parentPath"] = json!(parent.to_string_lossy());
                        output.push(value)
                    } else {
                        output.push(json!(entry.file_name().to_string_lossy()));
                    }
                }
                output.sort_by(|a, b| {
                    a["name"]
                        .as_str()
                        .or_else(|| a.as_str())
                        .cmp(&b["name"].as_str().or_else(|| b.as_str()))
                });
                Ok(json!(output))
            }
            "unlink" => {
                fs::remove_file(path_arg()?)?;
                Ok(Value::Null)
            }
            "rmdir" => {
                fs::remove_dir(path_arg()?)?;
                Ok(Value::Null)
            }
            "rm" => {
                let path = path_arg()?;
                let result = match fs::symlink_metadata(&path) {
                    Ok(meta) if meta.is_dir() => {
                        if args["recursive"] == true {
                            fs::remove_dir_all(&path)
                        } else {
                            Err(io::Error::new(
                                io::ErrorKind::IsADirectory,
                                "Path is a directory",
                            ))
                        }
                    }
                    Ok(_) => fs::remove_file(&path),
                    Err(error) => Err(error),
                };
                match result {
                    Err(error)
                        if args["force"] == true && error.kind() == io::ErrorKind::NotFound => {}
                    other => other?,
                };
                Ok(Value::Null)
            }
            "rename" => {
                fs::rename(path_arg()?, path(&args["destination"])?)?;
                Ok(Value::Null)
            }
            "copyFile" => {
                let destination = path(&args["destination"])?;
                let mut input = open(&path_arg()?, "r", mode)?;
                if !input.metadata()?.is_file() {
                    return Err(invalid("copyFile requires a regular file"));
                }
                if args["exclusive"] != true && same_file(&input, &destination)? {
                    return Ok(Value::Null);
                }
                let mut output = open(
                    &destination,
                    if args["exclusive"] == true { "wx" } else { "w" },
                    input.metadata()?.permissions_mode(),
                )?;
                io::copy(&mut input, &mut output)?;
                Ok(Value::Null)
            }
            "link" => {
                fs::hard_link(path_arg()?, path(&args["destination"])?)?;
                Ok(Value::Null)
            }
            "symlink" => {
                #[cfg(unix)]
                {
                    std::os::unix::fs::symlink(path_arg()?, path(&args["destination"])?)?;
                    Ok(Value::Null)
                }
                #[cfg(not(unix))]
                {
                    Err(invalid("Symlink creation is unavailable on this target"))
                }
            }
            "truncate" => {
                open(&path_arg()?, "r+", mode)?.set_len(args["length"].as_u64().unwrap_or(0))?;
                Ok(Value::Null)
            }
            "chmod" => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(path_arg()?, fs::Permissions::from_mode(mode))?;
                    Ok(Value::Null)
                }
                #[cfg(not(unix))]
                {
                    Err(invalid("chmod is unavailable on this target"))
                }
            }
            "access" => {
                let path = path_arg()?;
                let flags = args["mode"].as_u64().unwrap_or(0) as i32;
                #[cfg(unix)]
                {
                    use std::os::unix::ffi::OsStrExt;
                    let path = std::ffi::CString::new(path.as_os_str().as_bytes())
                        .map_err(|_| invalid("Path contains null bytes"))?;
                    if unsafe { libc::access(path.as_ptr(), flags) } != 0 {
                        return Err(io::Error::last_os_error());
                    }
                }
                #[cfg(not(unix))]
                {
                    let _ = flags;
                    fs::metadata(path)?;
                }
                Ok(Value::Null)
            }
            _ => Err(invalid("Unsupported filesystem operation")),
        }
    }
}
trait MetadataMode {
    fn permissions_mode(&self) -> u32;
}
impl MetadataMode for fs::Metadata {
    fn permissions_mode(&self) -> u32 {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            self.permissions().mode()
        }
        #[cfg(not(unix))]
        {
            0o666
        }
    }
}
#[path = "runtime_icu.rs"]
mod icu;
enum Decoder {
    Portable(encoding_rs::Decoder),
    Icu(icu::Decoder),
}
pub struct Codecs {
    streams: BTreeMap<u64, Decoder>,
    next: u64,
    icu: Option<std::sync::Arc<icu::Library>>,
}
impl Default for Codecs {
    fn default() -> Self {
        Self {
            streams: BTreeMap::new(),
            next: 0,
            icu: icu::Library::load(),
        }
    }
}
impl Codecs {
    pub fn call(&mut self, request: &str) -> String {
        let args: Value = match serde_json::from_str(request) {
            Ok(args) => args,
            Err(_) => return codec(request),
        };
        if !matches!(
            args["operation"].as_str(),
            Some("decoder_label" | "decoder_decode" | "decoder_info")
        ) {
            return codec(request);
        }
        let result = (|| -> io::Result<Value> {
            if args["operation"] == "decoder_info" {
                let backends: serde_json::Map<String, Value> = [
                    "utf-16le",
                    "utf-16be",
                    "big5",
                    "shift_jis",
                    "euc-jp",
                    "euc-kr",
                    "gbk",
                    "gb18030",
                    "iso-2022-jp",
                ]
                .into_iter()
                .map(|encoding| {
                    let native = self
                        .icu
                        .as_ref()
                        .is_some_and(|library| library.decoder(encoding, false, false).is_some());
                    (
                        encoding.to_owned(),
                        json!(if native { "icu" } else { "encoding_rs" }),
                    )
                })
                .collect();
                return Ok(
                    json!({"portable":"encoding_rs 0.8.35","encodings":backends,"icuVersion":self.icu.as_ref().map(|icu|&icu.version),"icuLibrary":self.icu.as_ref().map(|icu|&icu.path),"privateMappingStatus":self.icu.as_ref().map(|icu|icu.private_mapping_status()),"limits":{"inputBytes":LIMIT,"outputUtf8Bytes":LIMIT,"activeStreams":1024,"icuOutputAllocationBytes":32*1024*1024},"fallbackInstalledMultibyteParity":false}),
                );
            }
            let label = args["encoding"].as_str().unwrap_or("utf-8");
            let Some(encoding) = encoding_rs::Encoding::for_label(label.as_bytes()) else {
                return Ok(json!({"unsupported":true}));
            };
            let name = encoding.name().to_ascii_lowercase();
            if args["operation"] == "decoder_label" {
                return Ok(if std::ptr::eq(encoding, encoding_rs::REPLACEMENT) {
                    json!({"encoding":name,"unsupported":true})
                } else {
                    json!({"encoding":name})
                });
            }
            let input = bytes(&args["data"])?;
            let id = args["id"].as_u64();
            let mut decoder = match id {
                Some(id) => self
                    .streams
                    .remove(&id)
                    .ok_or_else(|| invalid("TextDecoder stream is no longer available"))?,
                None => {
                    let native = if matches!(
                        name.as_str(),
                        "utf-16le"
                            | "utf-16be"
                            | "big5"
                            | "shift_jis"
                            | "euc-jp"
                            | "euc-kr"
                            | "gbk"
                            | "gb18030"
                            | "iso-2022-jp"
                    ) {
                        self.icu.as_ref().and_then(|library| {
                            library.decoder(&name, args["fatal"] == true, args["ignoreBOM"] == true)
                        })
                    } else {
                        None
                    };
                    match native {
                        Some(decoder) => Decoder::Icu(decoder),
                        None => Decoder::Portable(if args["ignoreBOM"] == true {
                            encoding.new_decoder_without_bom_handling()
                        } else {
                            encoding.new_decoder_with_bom_removal()
                        }),
                    }
                }
            };
            let last = args["stream"] != true;
            let (output, invalid_data) = match &mut decoder {
                Decoder::Icu(decoder) => decoder.decode(&input, last)?,
                // encoding_rs 0.8.35 clears a pending two-byte lead on an empty call.
                // An empty streaming chunk must leave all decoder state untouched.
                Decoder::Portable(_) if input.is_empty() && !last => (String::new(), false),
                Decoder::Portable(decoder) => {
                    let capacity = decoder
                        .max_utf8_buffer_length(input.len())
                        .ok_or_else(|| invalid("Decoded text exceeds 8 MiB"))?
                        .min(LIMIT);
                    let mut output = String::with_capacity(capacity);
                    let invalid_data = if args["fatal"] == true {
                        let (result, read) =
                            decoder.decode_to_string_without_replacement(&input, &mut output, last);
                        match result {
                            encoding_rs::DecoderResult::InputEmpty if read == input.len() => false,
                            encoding_rs::DecoderResult::Malformed(_, _) => true,
                            _ => return Err(invalid("Decoded text exceeds its allocated bound")),
                        }
                    } else {
                        let (result, read, _) = decoder.decode_to_string(&input, &mut output, last);
                        if result != encoding_rs::CoderResult::InputEmpty || read != input.len() {
                            return Err(invalid("Decoded text exceeds its allocated bound"));
                        }
                        false
                    };
                    (output, invalid_data)
                }
            };
            if invalid_data && std::ptr::eq(encoding, encoding_rs::UTF_8) {
                decoder = Decoder::Portable(encoding.new_decoder_without_bom_handling());
            }
            let stream_id = if !last {
                if self.streams.len() >= 1024 {
                    return Err(invalid("TextDecoder active stream limit exceeded"));
                }
                let id = match id {
                    Some(id) => id,
                    None => {
                        self.next = self
                            .next
                            .checked_add(1)
                            .ok_or_else(|| invalid("TextDecoder stream identity exhausted"))?;
                        self.next
                    }
                };
                self.streams.insert(id, decoder);
                Some(id)
            } else {
                None
            };
            Ok(json!({"text":output,"id":stream_id,"invalid":invalid_data}))
        })();
        match result {Ok(value)=>json!({"result":value}),Err(error)=>json!({"error":{"code":"ERR_ENCODING_INVALID_ENCODED_DATA","message":error.to_string()}})}.to_string()
    }
}
pub fn codec(request: &str) -> String {
    let result = (|| -> io::Result<Value> {
        let args: Value =
            serde_json::from_str(request).map_err(|_| invalid("Invalid codec request"))?;
        if let Some(operation) = args["operation"].as_str() {
            let domain = args["domain"]
                .as_str()
                .ok_or_else(|| invalid("Domain must be a string"))?;
            if domain.len() > 128 * 1024 {
                return Err(invalid("Domain exceeds 128 KiB"));
            }
            let mut parsed = url::Url::parse("http://domain.invalid").expect("constant URL");
            if domain.is_empty() || url::quirks::set_hostname(&mut parsed, domain).is_err() {
                return Ok(json!(""));
            }
            let ascii = parsed.host_str().unwrap_or("");
            return match operation {
                "domainToASCII" => Ok(json!(ascii)),
                "domainToUnicode" => Ok(json!(
                    if matches!(parsed.host(), Some(url::Host::Domain(_))) {
                        url::quirks::domain_to_unicode(ascii)
                    } else {
                        ascii.to_string()
                    }
                )),
                _ => Err(invalid("Unknown URL operation")),
            };
        }
        let bytes = bytes(&args["data"])?;
        let encoding = args["encoding"]
            .as_str()
            .unwrap_or("utf8")
            .to_ascii_lowercase();
        let text = match encoding.as_str() {
            "utf8" | "utf-8" => {
                if args["fatal"] == true {
                    std::str::from_utf8(&bytes)
                        .map_err(|_| invalid("The encoded data was not valid UTF-8"))?
                        .to_string()
                } else {
                    String::from_utf8_lossy(&bytes).into_owned()
                }
            }
            "hex" => bytes.iter().map(|b| format!("{b:02x}")).collect(),
            "base64" => base64::engine::general_purpose::STANDARD.encode(&bytes),
            "base64url" => base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&bytes),
            "latin1" | "binary" => bytes.iter().map(|b| char::from(*b)).collect(),
            "ascii" => bytes.iter().map(|b| char::from(*b & 127)).collect(),
            "utf16le" | "utf-16le" | "ucs2" | "ucs-2" => String::from_utf16_lossy(
                &bytes
                    .chunks_exact(2)
                    .map(|b| u16::from_le_bytes([b[0], b[1]]))
                    .collect::<Vec<_>>(),
            ),
            _ => return Err(invalid("Unknown encoding")),
        };
        Ok(json!(text))
    })();
    match result{Ok(value)=>json!({"result":value}),Err(error)=>json!({"error":{"code":"ERR_ENCODING_INVALID_ENCODED_DATA","message":error.to_string()}})}.to_string()
}

#[cfg(test)]
mod decoder_tests {
    use super::*;
    fn portable() -> Codecs {
        Codecs {
            streams: BTreeMap::new(),
            next: 0,
            icu: None,
        }
    }
    fn decode(codecs: &mut Codecs, encoding: &str, data: &[u8], id: Value, stream: bool) -> Value {
        serde_json::from_str(&codecs.call(&json!({"operation":"decoder_decode","encoding":encoding,"data":base64::engine::general_purpose::STANDARD.encode(data),"id":id,"stream":stream}).to_string())).unwrap()
    }
    #[test]
    fn portable_decoder_keeps_split_characters_across_empty_chunks_and_flushes() {
        for (encoding, first, last, text) in [
            ("utf-8", vec![0xf0, 0x9f], vec![0xa7, 0xaa], "🧪"),
            ("shift_jis", vec![0x82], vec![0xa0], "あ"),
            ("utf-16le", vec![0x41], vec![0], "A"),
        ] {
            let mut codecs = portable();
            let started = decode(&mut codecs, encoding, &first, Value::Null, true);
            assert_eq!(started["result"]["text"], "");
            let empty = decode(
                &mut codecs,
                encoding,
                &[],
                started["result"]["id"].clone(),
                true,
            );
            assert_eq!(empty["result"]["text"], "");
            let complete = decode(
                &mut codecs,
                encoding,
                &last,
                empty["result"]["id"].clone(),
                false,
            );
            assert_eq!(complete["result"]["text"], text, "{encoding}: {complete}");
            assert!(codecs.streams.is_empty());
        }
    }
    #[test]
    fn decoder_stream_ownership_release_and_allocation_budget_are_enforced() {
        let mut one = portable();
        let mut other = portable();
        let first = decode(&mut one, "utf-8", &[], Value::Null, true);
        let id = first["result"]["id"].clone();
        assert!(
            decode(&mut other, "utf-8", b"x", id.clone(), false)
                .get("error")
                .is_some()
        );
        assert_eq!(
            decode(&mut one, "utf-8", b"x", id.clone(), false)["result"]["text"],
            "x"
        );
        assert!(
            decode(&mut one, "utf-8", b"x", id, false)
                .get("error")
                .is_some()
        );
        for _ in 0..1024 {
            assert!(
                decode(&mut one, "utf-8", &[], Value::Null, true)
                    .get("error")
                    .is_none()
            );
        }
        assert!(
            decode(&mut one, "utf-8", &[], Value::Null, true)["error"]["message"]
                .as_str()
                .unwrap()
                .contains("stream limit")
        );
        assert_eq!(one.streams.len(), 1024);
    }
}
