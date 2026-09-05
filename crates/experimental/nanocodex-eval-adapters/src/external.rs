use std::{
    fs,
    path::{Component, Path, PathBuf},
    time::Duration,
};

use serde::Deserialize;

use nanocodex_eval::{
    Resources, TaskOutput,
    import::{
        CasePlan, DatasetImporter, DatasetPlan, Environment, Harness, ImportError, SourceIdentity,
    },
};

use crate::{safe_case_id, sha256_file};

/// Importer for benchmark-owned hermetic harness manifests.
///
/// This is the boundary for PaperBench, MLE-style competitions, and private
/// suites whose official grader is executable code. Nanocodex owns the VM
/// lifecycle; the benchmark continues to own its image and `test.sh`.
#[derive(Clone, Debug)]
pub struct ExternalHarness {
    manifest: PathBuf,
}

impl ExternalHarness {
    /// Creates an importer from a TOML harness manifest.
    #[must_use]
    pub fn new(manifest: impl Into<PathBuf>) -> Self {
        Self {
            manifest: manifest.into(),
        }
    }
}

impl DatasetImporter for ExternalHarness {
    fn plan(&self) -> Result<DatasetPlan, ImportError> {
        let text = fs::read_to_string(&self.manifest).map_err(|source| ImportError::Io {
            path: self.manifest.clone(),
            source,
        })?;
        let raw: ExternalManifest = toml::from_str(&text).map_err(|source| {
            ImportError::Invalid(format!(
                "failed to decode {}: {source}",
                self.manifest.display()
            ))
        })?;
        if raw.schema_version != "1" {
            return Err(ImportError::Invalid(format!(
                "unsupported external harness schema {:?}",
                raw.schema_version
            )));
        }
        let root = self.manifest.parent().ok_or_else(|| {
            ImportError::Invalid("external manifest has no parent directory".to_owned())
        })?;
        let root = fs::canonicalize(root).map_err(|source| ImportError::Io {
            path: root.to_path_buf(),
            source,
        })?;
        let source = SourceIdentity::new(
            raw.source.kind,
            raw.source.revision,
            sha256_file(&self.manifest)?,
        )?;
        let mut plan = DatasetPlan::new(raw.name, source)?;
        for case in raw.cases {
            let environment = match (case.oci_image, case.environment) {
                (Some(image), None) => Environment::OciImage(image),
                (None, Some(directory)) => {
                    Environment::Dockerfile(resolve_source(&root, &directory, "environment")?)
                }
                _ => {
                    return Err(ImportError::Invalid(format!(
                        "case {:?} must select exactly one of oci_image or environment",
                        case.id
                    )));
                }
            };
            let output = match case.output.as_str() {
                "workspace" => TaskOutput::Workspace,
                "final_message" => TaskOutput::FinalMessage,
                output => {
                    return Err(ImportError::Invalid(format!(
                        "case {:?} has unsupported output {output:?}",
                        case.id
                    )));
                }
            };
            let resources = case.resources.unwrap_or_default();
            let mut planned = CasePlan::hermetic(
                safe_case_id(&case.id),
                case.prompt,
                environment,
                Harness::directory(resolve_source(&root, &case.harness, "harness")?)?,
            )?
            .output(output)
            .resources(Resources {
                cpus: resources.cpus,
                memory_mb: resources.memory_mb,
                storage_mb: resources.storage_mb,
                gpus: resources.gpus,
            })
            .timeouts(
                Duration::from_secs(case.agent_timeout_sec),
                Duration::from_secs(case.verifier_timeout_sec),
            )
            .allow_internet(case.allow_internet);
            for file in case.files {
                let source = resolve_source(&root, &file.source, "case file")?;
                let bytes = fs::read(&source).map_err(|source_error| ImportError::Io {
                    path: source,
                    source: source_error,
                })?;
                planned = planned.harness_file(file.destination, bytes, file.mode)?;
            }
            plan = plan.case(planned);
        }
        Ok(plan)
    }
}

fn resolve_source(root: &Path, relative: &Path, field: &str) -> Result<PathBuf, ImportError> {
    if relative.as_os_str().is_empty()
        || relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(ImportError::Invalid(format!(
            "external {field} must be a safe manifest-relative path: {}",
            relative.display()
        )));
    }
    let requested = root.join(relative);
    let resolved = fs::canonicalize(&requested).map_err(|source| ImportError::Io {
        path: requested,
        source,
    })?;
    if !resolved.starts_with(root) {
        return Err(ImportError::Invalid(format!(
            "external {field} escapes the manifest directory: {}",
            relative.display()
        )));
    }
    Ok(resolved)
}

#[derive(Deserialize)]
struct ExternalManifest {
    schema_version: String,
    name: String,
    source: ExternalSource,
    #[serde(rename = "case")]
    cases: Vec<ExternalCase>,
}

#[derive(Deserialize)]
struct ExternalSource {
    kind: String,
    revision: String,
}

#[derive(Deserialize)]
struct ExternalCase {
    id: String,
    prompt: String,
    output: String,
    #[serde(default)]
    oci_image: Option<String>,
    #[serde(default)]
    environment: Option<PathBuf>,
    harness: PathBuf,
    #[serde(default, rename = "file")]
    files: Vec<ExternalFile>,
    #[serde(default = "enabled")]
    allow_internet: bool,
    #[serde(default = "default_agent_timeout")]
    agent_timeout_sec: u64,
    #[serde(default = "default_verifier_timeout")]
    verifier_timeout_sec: u64,
    #[serde(default)]
    resources: Option<ExternalResources>,
}

#[derive(Deserialize)]
struct ExternalFile {
    source: PathBuf,
    destination: PathBuf,
    #[serde(default = "default_file_mode")]
    mode: u32,
}

#[derive(Deserialize)]
struct ExternalResources {
    #[serde(default = "default_cpus")]
    cpus: u32,
    #[serde(default = "default_memory")]
    memory_mb: u64,
    #[serde(default = "default_storage")]
    storage_mb: u64,
    #[serde(default)]
    gpus: u32,
}

impl Default for ExternalResources {
    fn default() -> Self {
        Self {
            cpus: default_cpus(),
            memory_mb: default_memory(),
            storage_mb: default_storage(),
            gpus: 0,
        }
    }
}

const fn enabled() -> bool {
    true
}

const fn default_cpus() -> u32 {
    2
}

const fn default_memory() -> u64 {
    4096
}

const fn default_storage() -> u64 {
    10_240
}

const fn default_agent_timeout() -> u64 {
    900
}

const fn default_verifier_timeout() -> u64 {
    300
}

const fn default_file_mode() -> u32 {
    0o600
}

#[cfg(test)]
mod tests {
    use std::fs;

    use nanocodex_eval::import::ImportStore;

    use super::*;

    #[test]
    fn keeps_benchmark_harness_out_of_vm_policy() {
        let source = tempfile::tempdir().unwrap();
        let harness = source.path().join("harness");
        fs::create_dir(&harness).unwrap();
        fs::write(
            harness.join("test.sh"),
            "#!/bin/sh\nprintf '1\\n' > /logs/verifier/reward.txt\n",
        )
        .unwrap();
        fs::write(source.path().join("paper-1.json"), r#"{"paper":"1"}"#).unwrap();
        let manifest = source.path().join("paperbench.toml");
        fs::write(
            &manifest,
            r#"schema_version = "1"
name = "paperbench"

[source]
kind = "paperbench"
revision = "paperbench@abc"

[[case]]
id = "paper-1"
prompt = "Reproduce the paper."
output = "workspace"
oci_image = "paperbench/environment@sha256:abc"
harness = "harness"

[[case.file]]
source = "paper-1.json"
destination = "case.json"
"#,
        )
        .unwrap();
        let store = tempfile::tempdir().unwrap();

        let dataset = ImportStore::new(store.path())
            .import(&ExternalHarness::new(manifest))
            .unwrap();

        assert_eq!(dataset.source().kind(), "paperbench");
        assert_eq!(dataset.tasks()[0].prompt(), "Reproduce the paper.");
        assert_eq!(
            fs::read_to_string(dataset.tasks()[0].root().join("tests/case.json")).unwrap(),
            r#"{"paper":"1"}"#
        );
    }

    #[test]
    fn rejects_sources_outside_its_bundle() {
        let bundle = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::create_dir(outside.path().join("harness")).unwrap();
        fs::write(
            outside.path().join("harness/test.sh"),
            "#!/bin/sh\nprintf '1\\n' > /logs/verifier/reward.txt\n",
        )
        .unwrap();
        let manifest = bundle.path().join("unsafe.toml");
        fs::write(
            &manifest,
            r#"schema_version = "1"
name = "unsafe"

[source]
kind = "private"
revision = "private@abc"

[[case]]
id = "case"
prompt = "Do work."
output = "workspace"
oci_image = "debian:bookworm-slim"
harness = "../harness"
"#,
        )
        .unwrap();

        let error = ExternalHarness::new(manifest).plan().unwrap_err();
        assert!(error.to_string().contains("safe manifest-relative path"));
    }
}
