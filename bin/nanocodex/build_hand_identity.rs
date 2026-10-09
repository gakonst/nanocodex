//! Deterministic identity of the standalone Hand (nanocodex-hand) build.
//!
//! The identity is a SHA-256 over length-prefixed, sorted records of every
//! conservative source and toolchain input set for the Hand executable:
//!
//! * file contents (by workspace-relative path) of every path package in the
//!   Cargo.lock closure of nanocodex-bin, minus the explicit CLI-only paths in
//!   hand-identity.toml (the CLI executable root and terminal renderers);
//! * the closure's resolved Cargo.lock entries (name, version, source,
//!   checksum, dependency edges) and the workspace manifest/config files;
//! * compiler (rustc -vV), target, profile, optimisation/debug settings,
//!   encoded rustflags, cfgs, enabled features and C toolchain variables;
//! * the embedded Linux screen-helper payload bytes and the macOS menu-bar
//!   helper sources and its Swift compiler version.
//!
//! It deliberately excludes timestamps, Git revisions, absolute checkout paths
//! and file modification times. Anything not recognised is included, so an
//! unknown input can only change the identity, never leave it stale.

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use sha2::{Digest, Sha256};

const SCHEMA: &str = "nanocodex-hand-identity-v1";
const MANIFEST: &str = "hand-identity.toml";
const ROOT_PACKAGE: &str = "nanocodex-bin";

/// Directory names never hashed: build outputs, VCS and JS dependency trees.
const SKIPPED_DIRECTORIES: &[&str] = &["target", ".git", "node_modules"];

/// Environment variables that change compiler, linker or C dependency output.
const TOOLCHAIN_ENV: &[&str] = &[
    "TARGET",
    "HOST",
    "PROFILE",
    "OPT_LEVEL",
    "DEBUG",
    "CARGO_ENCODED_RUSTFLAGS",
    "CARGO_PKG_VERSION",
    "CC",
    "CXX",
    "AR",
    "CFLAGS",
    "CXXFLAGS",
    "LDFLAGS",
    "TARGET_CC",
    "TARGET_CXX",
    "TARGET_AR",
    "TARGET_CFLAGS",
    "TARGET_CXXFLAGS",
    "MACOSX_DEPLOYMENT_TARGET",
    "SDKROOT",
    "NANOCODEX_LINUX_SCREEN_BUNDLE_REQUIRED",
];

struct Hasher {
    records: BTreeMap<(String, String), Vec<u8>>,
}

impl Hasher {
    fn record(&mut self, kind: &str, name: impl Into<String>, value: impl Into<Vec<u8>>) {
        self.records
            .insert((kind.to_owned(), name.into()), value.into());
    }

    fn finish(self, inputs_report: &Path) -> Result<String, Box<dyn Error>> {
        let mut digest = Sha256::new();
        let mut report = format!("{SCHEMA}\n");
        for ((kind, name), value) in &self.records {
            for part in [kind.as_bytes(), name.as_bytes(), value.as_slice()] {
                digest.update((part.len() as u64).to_le_bytes());
                digest.update(part);
            }
            let _ = writeln!(report, "{kind}\t{name}\t{}", hex(&Sha256::digest(value)));
        }
        let identity = hex(&digest.finalize());
        let _ = writeln!(report, "identity\t{identity}");
        fs::write(inputs_report, report)?;
        Ok(identity)
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(64), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

fn env(name: &str) -> Option<String> {
    println!("cargo:rerun-if-env-changed={name}");
    std::env::var(name).ok()
}

/// Computes the identity and exports it as NANOCODEX_HAND_IDENTITY. Call after
/// the embedded payloads have been staged in OUT_DIR.
pub fn emit() -> Result<(), Box<dyn Error>> {
    let manifest_dir =
        PathBuf::from(env("CARGO_MANIFEST_DIR").ok_or("CARGO_MANIFEST_DIR missing")?);
    let out_dir = PathBuf::from(env("OUT_DIR").ok_or("OUT_DIR missing")?);
    let root = manifest_dir
        .join("../..")
        .canonicalize()
        .map_err(|error| format!("cannot resolve the workspace root: {error}"))?;
    let mut hasher = Hasher {
        records: BTreeMap::new(),
    };
    hasher.record("schema", SCHEMA, SCHEMA);

    let manifest_path = manifest_dir.join(MANIFEST);
    println!("cargo:rerun-if-changed={}", manifest_path.display());
    let manifest: toml::Table = toml::from_str(&fs::read_to_string(&manifest_path)?)?;
    let excluded = string_list(&manifest, "exclude")?;
    let inputs = string_list(&manifest, "inputs")?;
    let optional_inputs = string_list(&manifest, "optional_inputs")?;
    for path in &excluded {
        if !root.join(path).exists() {
            return Err(
                format!("{MANIFEST} excludes missing path {path}; update the manifest").into(),
            );
        }
    }

    // Resolved dependency closure from Cargo.lock.
    let lock_path = root.join("Cargo.lock");
    println!("cargo:rerun-if-changed={}", lock_path.display());
    let lock: toml::Table = toml::from_str(&fs::read_to_string(&lock_path)?)?;
    let closure = lock_closure(&lock)?;
    let path_packages = path_package_directories(&root)?;
    let mut hashed_directories = BTreeSet::new();
    for (name, package) in &closure {
        hasher.record("lock", name.clone(), package.to_string());
        if package.get("source").is_none() {
            let package_name = package
                .get("name")
                .and_then(toml::Value::as_str)
                .ok_or("Cargo.lock package without a name")?;
            let directory = path_packages.get(package_name).ok_or_else(|| {
                format!("cannot locate path package {package_name} of the Hand closure; refusing an incomplete Hand identity")
            })?;
            hashed_directories.insert(directory.clone());
        }
    }
    let nested: BTreeSet<PathBuf> = path_packages.values().cloned().collect();
    for directory in &hashed_directories {
        println!("cargo:rerun-if-changed={}", root.join(directory).display());
        hash_tree(&root, directory, &nested, &excluded, &mut hasher)?;
    }

    for input in inputs.iter().chain(&optional_inputs) {
        let path = root.join(input);
        println!("cargo:rerun-if-changed={}", path.display());
        if path.is_dir() {
            hash_tree(
                &root,
                Path::new(input),
                &BTreeSet::new(),
                &excluded,
                &mut hasher,
            )?;
        } else if path.is_file() {
            hasher.record("file", input.clone(), fs::read(&path)?);
        } else if inputs.contains(input) {
            return Err(format!("Hand identity input {input} is missing").into());
        }
    }

    // Compiler, target, profile and build flags.
    let rustc = env("RUSTC").unwrap_or_else(|| String::from("rustc"));
    hasher.record("tool", "rustc -vV", command_output(&rustc, &["-vV"])?);
    for name in TOOLCHAIN_ENV {
        if let Some(value) = env(name) {
            hasher.record("env", *name, value);
        }
    }
    if let Ok(target) = std::env::var("TARGET") {
        let triple = target.replace(['-', '.'], "_");
        for name in [
            format!("CARGO_TARGET_{}_LINKER", triple.to_ascii_uppercase()),
            format!("CARGO_TARGET_{}_RUSTFLAGS", triple.to_ascii_uppercase()),
            format!("CC_{triple}"),
            format!("CXX_{triple}"),
            format!("AR_{triple}"),
            format!("CFLAGS_{triple}"),
            format!("CXXFLAGS_{triple}"),
        ] {
            if let Some(value) = env(&name) {
                hasher.record("env", name, value);
            }
        }
    }
    // build_version.rs derives the nightly channel (IS_NIGHTLY) from TAG_NAME.
    let nightly = env("TAG_NAME").is_some_and(|tag| tag.contains("nightly"));
    hasher.record(
        "env",
        "release channel",
        if nightly { "nightly" } else { "other" },
    );
    for (name, value) in std::env::vars() {
        if name.starts_with("CARGO_CFG_") || name.starts_with("CARGO_FEATURE_") {
            hasher.record("env", name, value);
        }
    }

    // Embedded payloads, exactly as staged for include_bytes!.
    hasher.record(
        "payload",
        "linux-screen-helpers.tar.gz",
        fs::read(out_dir.join("linux-screen-helpers.tar.gz"))?,
    );
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        hasher.record(
            "tool",
            "xcrun swiftc --version",
            command_output("xcrun", &["swiftc", "--version"])?,
        );
    }

    let identity = hasher.finish(&out_dir.join("hand-identity-inputs.tsv"))?;
    println!("cargo:rustc-env=NANOCODEX_HAND_IDENTITY={identity}");
    Ok(())
}

fn string_list(table: &toml::Table, key: &str) -> Result<Vec<String>, Box<dyn Error>> {
    let Some(value) = table.get(key) else {
        return Ok(Vec::new());
    };
    value
        .as_array()
        .ok_or_else(|| format!("{MANIFEST}: {key} must be an array"))?
        .iter()
        .map(|item| {
            item.as_str()
                .map(|path| path.trim_end_matches('/').to_owned())
                .ok_or_else(|| format!("{MANIFEST}: {key} entries must be strings").into())
        })
        .collect()
}

fn command_output(program: &str, arguments: &[&str]) -> Result<Vec<u8>, Box<dyn Error>> {
    let output = Command::new(program)
        .args(arguments)
        .output()
        .map_err(|error| format!("cannot run {program} for the Hand identity: {error}"))?;
    if !output.status.success() {
        return Err(format!("{program} {arguments:?} failed for the Hand identity").into());
    }
    Ok(output.stdout)
}

/// All Cargo.lock packages reachable from nanocodex-bin, keyed canonically.
fn lock_closure(lock: &toml::Table) -> Result<BTreeMap<String, toml::Value>, Box<dyn Error>> {
    let packages = lock
        .get("package")
        .and_then(toml::Value::as_array)
        .ok_or("Cargo.lock has no packages")?;
    let field = |package: &toml::Value, key: &str| {
        package
            .get(key)
            .and_then(toml::Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    let key = |package: &toml::Value| {
        format!(
            "{} {} {}",
            field(package, "name"),
            field(package, "version"),
            field(package, "source")
        )
    };
    let resolve = |reference: &str| -> Result<&toml::Value, Box<dyn Error>> {
        let mut parts = reference.splitn(3, ' ');
        let name = parts.next().unwrap_or("");
        let version = parts.next();
        let source = parts
            .next()
            .map(|source| source.trim_start_matches('(').trim_end_matches(')'));
        let mut matches = packages.iter().filter(|package| {
            field(package, "name") == name
                && version.is_none_or(|version| field(package, "version") == version)
                && source.is_none_or(|source| field(package, "source") == source)
        });
        let found = matches
            .next()
            .ok_or_else(|| format!("Cargo.lock dependency {reference} is unresolved"))?;
        if matches.next().is_some() {
            return Err(format!("Cargo.lock dependency {reference} is ambiguous").into());
        }
        Ok(found)
    };
    let mut closure = BTreeMap::new();
    let mut pending = vec![resolve(ROOT_PACKAGE)?];
    while let Some(package) = pending.pop() {
        if closure.insert(key(package), package.clone()).is_some() {
            continue;
        }
        if let Some(dependencies) = package.get("dependencies").and_then(toml::Value::as_array) {
            for dependency in dependencies {
                let reference = dependency
                    .as_str()
                    .ok_or("Cargo.lock dependency is not a string")?;
                pending.push(resolve(reference)?);
            }
        }
    }
    Ok(closure)
}

/// Maps local package names to workspace-relative directories, following the
/// workspace members and every path dependency or patch reachable from them.
fn path_package_directories(root: &Path) -> Result<BTreeMap<String, PathBuf>, Box<dyn Error>> {
    let mut packages = BTreeMap::new();
    let mut visited = BTreeSet::new();
    let mut pending = vec![PathBuf::new()];
    while let Some(directory) = pending.pop() {
        let directory = normalize(&directory);
        if !visited.insert(directory.clone()) {
            continue;
        }
        let manifest_path = root.join(&directory).join("Cargo.toml");
        let manifest: toml::Table = toml::from_str(
            &fs::read_to_string(&manifest_path)
                .map_err(|error| format!("cannot read {}: {error}", manifest_path.display()))?,
        )?;
        if let Some(name) = manifest
            .get("package")
            .and_then(|package| package.get("name"))
            .and_then(toml::Value::as_str)
            && let Some(previous) = packages.insert(name.to_owned(), directory.clone())
            && previous != directory
        {
            return Err(format!(
                "path package {name} found at {} and {}",
                previous.display(),
                directory.display()
            )
            .into());
        }
        if let Some(members) = manifest
            .get("workspace")
            .and_then(|workspace| workspace.get("members"))
            .and_then(toml::Value::as_array)
        {
            for member in members.iter().filter_map(toml::Value::as_str) {
                if member.contains(['*', '?', '[']) {
                    return Err(format!(
                        "workspace member glob {member} is unsupported by the Hand identity"
                    )
                    .into());
                }
                pending.push(directory.join(member));
            }
        }
        let mut paths = Vec::new();
        collect_dependency_paths(&toml::Value::Table(manifest), false, &mut paths);
        for path in paths {
            let candidate = directory.join(path);
            if root.join(&candidate).join("Cargo.toml").is_file() {
                pending.push(candidate);
            }
        }
    }
    Ok(packages)
}

fn collect_dependency_paths(value: &toml::Value, in_dependencies: bool, paths: &mut Vec<String>) {
    let Some(table) = value.as_table() else {
        return;
    };
    for (key, child) in table {
        if in_dependencies && key == "path" {
            if let Some(path) = child.as_str() {
                paths.push(path.to_owned());
            }
            continue;
        }
        let dependencies = in_dependencies
            || key.ends_with("dependencies")
            || key == "patch"
            || key == "replace"
            || key == "workspace";
        collect_dependency_paths(child, dependencies && key != "members", paths);
    }
}

fn normalize(path: &Path) -> PathBuf {
    let mut parts: Vec<std::ffi::OsString> = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                parts.pop();
            }
            std::path::Component::Normal(part) => parts.push(part.to_owned()),
            _ => {}
        }
    }
    parts.iter().collect()
}

fn relative_name(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn hash_tree(
    root: &Path,
    directory: &Path,
    nested_packages: &BTreeSet<PathBuf>,
    excluded: &[String],
    hasher: &mut Hasher,
) -> Result<(), Box<dyn Error>> {
    let mut pending = vec![directory.to_path_buf()];
    while let Some(current) = pending.pop() {
        let mut entries = fs::read_dir(root.join(&current))?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let relative = current.join(entry.file_name());
            let name = relative_name(&relative);
            if excluded
                .iter()
                .any(|path| name == *path || name.starts_with(&format!("{path}/")))
            {
                continue;
            }
            let kind = entry.file_type()?;
            if kind.is_dir() {
                let file_name = entry.file_name();
                if SKIPPED_DIRECTORIES.iter().any(|skip| file_name == *skip)
                    || entry.path().join("CACHEDIR.TAG").exists()
                    || (relative != directory && nested_packages.contains(&relative))
                {
                    continue;
                }
                pending.push(relative);
            } else if kind.is_symlink() {
                let target = entry.path().canonicalize()?;
                if !target.starts_with(root) || !target.is_file() {
                    return Err(format!("Hand identity source symlink {name} must resolve to a file inside the workspace").into());
                }
                println!("cargo:rerun-if-changed={}", target.display());
                hasher.record(
                    "symlink-target",
                    name.clone(),
                    relative_name(target.strip_prefix(root)?),
                );
                hasher.record("file", name, fs::read(target)?);
            } else {
                hasher.record("file", name, fs::read(entry.path())?);
            }
        }
    }
    Ok(())
}
