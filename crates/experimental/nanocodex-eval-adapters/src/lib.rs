//! Pinned third-party benchmark adapters for `nanocodex-eval`.
//!
//! Adapters acquire authoritative source material and normalize it into the
//! evaluator's canonical immutable task boundary. Execution, durable
//! scheduling, and claim fencing remain in `nanocodex-eval`.

#![deny(missing_docs, rustdoc::broken_intra_doc_links)]

mod agents_last_exam;
mod arc_agi_3;
mod arena_hard;
mod browsecomp;
mod external;
mod gdpval;
mod genebench_pro;
mod gpqa_diamond;
mod graphwalks;
mod harbor;
mod healthbench_professional;
mod mrcr;
mod openai_evals;
mod source;
mod swe_atlas_qna;
mod swe_bench;

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{BufReader, Read as _},
    path::{Path, PathBuf},
};

use nanocodex_eval::{
    ResolvedTask,
    import::{ImportError, ImportStore, ImportedDataset},
};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use sha2::{Digest as _, Sha256};
use source::SourceStore;

/// Installed adapter catalog bound to one durable evaluator state directory.
#[derive(Clone, Debug)]
pub struct AdapterCatalog {
    imports: PathBuf,
    sources: PathBuf,
}

/// Adapter source acquisition, normalization, or task selection failed.
#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    /// A selector did not use the stable `<benchmark>/<task>` shape.
    #[error("invalid benchmark selector {0:?}; expected <benchmark>/<task> or <benchmark>/*")]
    InvalidSelector(String),
    /// A profile selected the same benchmark task more than once.
    #[error("duplicate benchmark selector {0:?}")]
    DuplicateSelector(String),
    /// No installed adapter owns the benchmark name.
    #[error("no installed evaluation adapter owns benchmark {0:?}")]
    UnknownBenchmark(String),
    /// An installed dataset did not contain requested normalized tasks.
    #[error("benchmark {benchmark:?} has no normalized task(s): {tasks}")]
    MissingTasks {
        /// Selected benchmark.
        benchmark: String,
        /// Missing task names.
        tasks: String,
    },
    /// Adapter source acquisition failed.
    #[error("adapter source acquisition failed: {0}")]
    Source(String),
    /// Immutable dataset import failed.
    #[error(transparent)]
    Import(#[from] ImportError),
    /// A blocking adapter worker failed.
    #[error("evaluation adapter worker failed: {0}")]
    Worker(String),
    /// Adapter configuration could not be read or decoded.
    #[error("invalid adapter configuration: {0}")]
    Configuration(String),
}

#[derive(Clone, Debug)]
pub(crate) struct BenchmarkRequest {
    name: String,
    all: bool,
    tasks: BTreeSet<String>,
}

#[derive(Clone, Copy)]
struct InstalledAdapter {
    kind: &'static str,
    names: &'static [&'static str],
    import: fn(
        &BenchmarkRequest,
        &SourceStore,
        &ImportStore,
        Option<&AdapterConfiguration>,
    ) -> Result<ImportedDataset, AdapterError>,
    matches: fn(&str, &str) -> bool,
}

#[derive(Clone, Debug)]
struct AdapterConfiguration {
    root: PathBuf,
    value: toml::Value,
}

#[derive(Deserialize)]
struct AdapterManifest {
    #[serde(default)]
    benchmark: BTreeMap<String, toml::Value>,
}

const INSTALLED_ADAPTERS: &[InstalledAdapter] = &[
    InstalledAdapter {
        kind: "harbor",
        names: &["terminal-bench-2.1", "deep-swe-v1.1"],
        import: import_harbor,
        matches: matches_harbor_task,
    },
    InstalledAdapter {
        kind: "arena-hard",
        names: &["arena-hard-v2"],
        import: import_arena_hard,
        matches: exact_task,
    },
    InstalledAdapter {
        kind: "swe-bench",
        names: &["swe-bench-verified-smoke"],
        import: import_swe_bench,
        matches: exact_task,
    },
    InstalledAdapter {
        kind: "swe-atlas-qna",
        names: &["swe-atlas-qna"],
        import: import_swe_atlas,
        matches: matches_swe_atlas_task,
    },
    InstalledAdapter {
        kind: "genebench-pro",
        names: &["genebench-pro-public"],
        import: import_genebench_pro,
        matches: exact_task,
    },
    InstalledAdapter {
        kind: "graphwalks",
        names: &["graphwalks"],
        import: import_graphwalks,
        matches: exact_task,
    },
    InstalledAdapter {
        kind: "mrcr",
        names: &["mrcr-v2"],
        import: import_mrcr,
        matches: exact_task,
    },
    InstalledAdapter {
        kind: "healthbench-professional",
        names: &["healthbench-professional"],
        import: import_healthbench_professional,
        matches: exact_task,
    },
    InstalledAdapter {
        kind: "gdpval",
        names: &["gdpval"],
        import: import_gdpval,
        matches: exact_task,
    },
    InstalledAdapter {
        kind: "gpqa-diamond",
        names: &["gpqa-diamond"],
        import: import_gpqa_diamond,
        matches: exact_task,
    },
    InstalledAdapter {
        kind: "browsecomp",
        names: &["browsecomp"],
        import: import_browsecomp,
        matches: exact_task,
    },
    InstalledAdapter {
        kind: "arc-agi-3",
        names: &["arc-agi-3-public-smoke"],
        import: import_arc_agi_3,
        matches: exact_task,
    },
    InstalledAdapter {
        kind: "agents-last-exam",
        names: &["agents-last-exam"],
        import: import_agents_last_exam,
        matches: exact_task,
    },
    InstalledAdapter {
        kind: "openai-evals",
        names: &[],
        import: import_openai_evals,
        matches: exact_task,
    },
    InstalledAdapter {
        kind: "external",
        names: &[],
        import: import_external,
        matches: exact_task,
    },
];

const TERMINAL_BENCH_REVISION: &str = "5c8eadf1f393183288fa08b8f73ca9a469cc5e00";
const DEEP_SWE_REVISION: &str = "e016041a6ccf8da29906afc9a3f5a8df940a1f78";
const ARENA_HARD_REVISION: &str = "196f6b826783b3da7310e361a805fa36f0be83f3";
const SWE_VERIFIED_ROW_RESPONSE_SHA256: &str =
    "7c62220a467830a3a330dda51211ab4c1ba099124dffc8371fbec057933c47b8";
const SWE_ATLAS_REVISION: &str = "6de82c3603fb9e254170b440d7560441eb257176";
const GENEBENCH_PRO_REVISION: &str = "eb75a3c0996b3cedcc9af685bad02fd166848fa2";
const GENEBENCH_PRO_MANIFEST_SHA256: &str =
    "0e80d5dca9ac5211fb9dfa5c0ea8d26e9d557e2039c8f20b0f5a328ea3cd6c58";
const GENEBENCH_PRO_GRADER_SHA256: &str =
    "81a50853d1348237300ce90a7b48a9230b4edb5d1af30207c37f17f0de8bbb28";
const GENEBENCH_PRO_BASE: &str =
    "https://huggingface.co/datasets/openai/genebench-pro-public-package/resolve";
const GRAPHWALKS_REVISION: &str = "f338bb265735a56a79f4b0f5def722c9c3268ead";
const GRAPHWALKS_SHORT_SHA256: &str =
    "54036036c91d8e04bb2a5fcd9e36f8e2a852cacece5dfc2b1ee40e3a6182b516";
const GRAPHWALKS_LONG_SHA256: &str =
    "537879431c72a42e3b500f80efc3047e7facb90390b6063d33679b4320985911";
const MRCR_REVISION: &str = "f4c69fae7cf81f7ca26b9fee34b392a50f6b8a1d";
const MRCR_FILES: [(&str, &str); 6] = [
    (
        "2needle/2needle_0.parquet",
        "1c297b254bf64a31856b74918cd7db889a214503e0b67daa834e84f20df6aa93",
    ),
    (
        "2needle/2needle_1.parquet",
        "a5a1dc9ccc945623253d04d33c03d89aee2d676c88955ce368da2ab16a0ce94d",
    ),
    (
        "4needle/4needle_0.parquet",
        "4d4fa3d11ce064749de3cd039eef1a621e30a81c2c9b3e64f1df37f8afeaf312",
    ),
    (
        "4needle/4needle_1.parquet",
        "8dfdb94a208cf3eee73c4e7ac6ee8a5ccb7236c6934c13c6c5f67c0a9928cdf3",
    ),
    (
        "8needle/8needle_0.parquet",
        "65df601a2e0ae4a3cfb56920a6ef99f26c0de37c6b1018695e8aed684e6a94c1",
    ),
    (
        "8needle/8needle_1.parquet",
        "c80b19573bff1d38e1c157d6a0bdf9cfd1a8ab6372296174c9a7015e164189e3",
    ),
];
const HEALTHBENCH_PROFESSIONAL_REVISION: &str = "349962fd46dd02343a0d8a606491baf59154ea1a";
const HEALTHBENCH_PROFESSIONAL_SHA256: &str =
    "d44b08e6e952e04c945e2c406f02533d9e7a989a84e35820ee7efdff20c9e4e2";
const GDPVAL_REVISION: &str = "11e7900cdcac61bc4daf59e65feb238acda98fbf";
const GDPVAL_PARQUET_SHA256: &str =
    "f8422fab9b21d90c0ee5f0659842ab666d418cb8940842918f9f4b0df7ae0202";
const GPQA_REVISION: &str = "56686c06f5e19865c153de0fdb11be3890014df7";
const GPQA_ZIP_SHA256: &str = "461ae7329f15a3e35f8184d2dac24b990f34fdf12f366ca4062d8e6638cd08dc";
const GPQA_DIAMOND_SHA256: &str =
    "41d1213cd7a4998605a26c2798500652572007161b3a92817ba46b35befcd305";
const BROWSECOMP_REVISION: &str = "652c89d0ca9df547706735883097e9537d40dc47";
const BROWSECOMP_SHA256: &str = "7b24471cd5b3eb2a46830a14802b5c029ea62f488ff75a0f88af7923d1454abf";
const ARC_AGI_REVISION: &str = "f12822c4d550121c35a275008d964afbbed47d2f";
const ARC_AGI_3_BENCHMARKING_REVISION: &str = "86d72170ce3155551712a9fafd290bab471d6eee";
const AGENTS_LAST_EXAM_REVISION: &str = "1e615e456de7cef57706680613cb80ee13c7fc76";
const AGENTS_LAST_EXAM_DATA_REVISION: &str = "5ae9b719a901c14a9ccec7b3bd156d663e3eedcb";
const AGENTS_LAST_EXAM_IMAGE: &str = "agentslastexam/ale-ubuntu22-docker@sha256:78ec11afeb0008ed8bc2b59cf9c90c05e63d1ac66b9d3e7cb0fada10695fca6f";

fn import_harbor(
    request: &BenchmarkRequest,
    sources: &SourceStore,
    store: &ImportStore,
    _configuration: Option<&AdapterConfiguration>,
) -> Result<ImportedDataset, AdapterError> {
    let (root, revision) = match request.name.as_str() {
        "terminal-bench-2.1" => (
            sources
                .git_checkout(
                    "terminal-bench-2-1",
                    "https://github.com/harbor-framework/terminal-bench-2-1.git",
                    TERMINAL_BENCH_REVISION,
                )?
                .join("tasks"),
            format!("harbor-framework/terminal-bench-2-1@{TERMINAL_BENCH_REVISION}"),
        ),
        "deep-swe-v1.1" => (
            sources
                .git_checkout(
                    "deep-swe",
                    "https://github.com/datacurve-ai/deep-swe.git",
                    DEEP_SWE_REVISION,
                )?
                .join("tasks"),
            format!("datacurve-ai/deep-swe@{DEEP_SWE_REVISION}"),
        ),
        _ => return Err(AdapterError::UnknownBenchmark(request.name.clone())),
    };
    Ok(store.import(&harbor::HarborDataset::new(&request.name, root, revision))?)
}

fn matches_harbor_task(selected: &str, normalized: &str) -> bool {
    selected == normalized
        || normalized
            .strip_prefix("terminal-bench/")
            .is_some_and(|task| task == selected)
        || normalized
            .strip_prefix("datacurve/")
            .is_some_and(|task| task == selected)
}

fn import_arena_hard(
    request: &BenchmarkRequest,
    sources: &SourceStore,
    store: &ImportStore,
    _configuration: Option<&AdapterConfiguration>,
) -> Result<ImportedDataset, AdapterError> {
    let source = sources.git_checkout(
        "arena-hard-auto",
        "https://github.com/lm-sys/arena-hard-auto.git",
        ARENA_HARD_REVISION,
    )?;
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/arena-hard");
    let importer = arena_hard::ArenaHard::new(
        &request.name,
        source.join("data/arena-hard-v2.0/question.jsonl"),
        format!("lm-sys/arena-hard-auto@{ARENA_HARD_REVISION}"),
        nanocodex_eval::import::Environment::OciImage("debian:bookworm-slim".to_owned()),
        nanocodex_eval::import::Harness::directory(assets)?,
    )
    .baseline_answers(source.join("data/arena-hard-v2.0/model_answer/o3-mini-2025-01-31.jsonl"));
    Ok(store.import(&importer)?)
}

fn import_swe_bench(
    request: &BenchmarkRequest,
    sources: &SourceStore,
    store: &ImportStore,
    _configuration: Option<&AdapterConfiguration>,
) -> Result<ImportedDataset, AdapterError> {
    let response = sources.download(
        "swe-bench/swe-bench-verified-smoke.response.json",
        "https://datasets-server.huggingface.co/rows?dataset=princeton-nlp/SWE-bench_Verified&config=default&split=test&offset=0&length=1",
        SWE_VERIFIED_ROW_RESPONSE_SHA256,
    )?;
    let document: serde_json::Value =
        serde_json::from_slice(&fs::read(&response).map_err(|error| {
            AdapterError::Source(format!("failed to read {}: {error}", response.display()))
        })?)
        .map_err(|error| AdapterError::Source(error.to_string()))?;
    let row = document
        .get("rows")
        .and_then(serde_json::Value::as_array)
        .and_then(|rows| rows.first())
        .and_then(|entry| entry.get("row"))
        .ok_or_else(|| AdapterError::Source("SWE-bench response has no first row".to_owned()))?;
    let mut bytes =
        serde_json::to_vec(row).map_err(|error| AdapterError::Source(error.to_string()))?;
    bytes.push(b'\n');
    let instances = sources.write_verified("swe-bench/swe-bench-verified-smoke.jsonl", &bytes)?;
    let harness = nanocodex_eval::import::Harness::directory(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/swe-bench"),
    )?;
    let importer = swe_bench::SweBench::new(
        &request.name,
        instances,
        "princeton-nlp/SWE-bench_Verified@c104f840cc67f8b6eec6f759ebc8b2693d585d4a",
        "swebench",
        harness,
    );
    Ok(store.import(&importer)?)
}

fn import_swe_atlas(
    _request: &BenchmarkRequest,
    sources: &SourceStore,
    store: &ImportStore,
    _configuration: Option<&AdapterConfiguration>,
) -> Result<ImportedDataset, AdapterError> {
    let root = sources.git_checkout(
        "swe-atlas",
        "https://github.com/scaleapi/SWE-Atlas.git",
        SWE_ATLAS_REVISION,
    )?;
    Ok(store.import(&swe_atlas_qna::SweAtlasQna::new(
        root.join("data/qa"),
        format!("scaleapi/SWE-Atlas@{SWE_ATLAS_REVISION}"),
    ))?)
}

fn matches_swe_atlas_task(selected: &str, normalized: &str) -> bool {
    selected == normalized
        || normalized
            .strip_prefix("scale-ai-")
            .is_some_and(|task| task == selected)
}

fn import_genebench_pro(
    _request: &BenchmarkRequest,
    sources: &SourceStore,
    store: &ImportStore,
    _configuration: Option<&AdapterConfiguration>,
) -> Result<ImportedDataset, AdapterError> {
    let package_name = "genebench-pro-public-package";
    let manifest_relative = format!("{package_name}/manifest.json");
    let manifest = sources.download(
        &manifest_relative,
        &format!("{GENEBENCH_PRO_BASE}/{GENEBENCH_PRO_REVISION}/manifest.json"),
        GENEBENCH_PRO_MANIFEST_SHA256,
    )?;
    sources.download(
        &format!("{package_name}/reference_grader.py"),
        &format!("{GENEBENCH_PRO_BASE}/{GENEBENCH_PRO_REVISION}/reference_grader.py"),
        GENEBENCH_PRO_GRADER_SHA256,
    )?;
    let bytes = fs::read(&manifest).map_err(|error| {
        AdapterError::Source(format!("failed to read {}: {error}", manifest.display()))
    })?;
    let package_manifest =
        genebench_pro::decode_manifest(&manifest, &bytes).map_err(AdapterError::Source)?;
    for problem in package_manifest.problems {
        for file in problem.execution_files() {
            let relative = Path::new(&file.path);
            if relative.is_absolute()
                || relative
                    .components()
                    .any(|component| !matches!(component, std::path::Component::Normal(_)))
            {
                return Err(AdapterError::Source(format!(
                    "GeneBench-Pro manifest contains unsafe path {:?}",
                    file.path
                )));
            }
            sources.download(
                &format!("{package_name}/{}", file.path),
                &format!(
                    "{GENEBENCH_PRO_BASE}/{GENEBENCH_PRO_REVISION}/{}",
                    file.path
                ),
                &file.sha256,
            )?;
        }
    }
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/genebench-pro");
    Ok(store.import(&genebench_pro::GeneBenchPro::new(
        sources.root().join(package_name),
        format!("openai/genebench-pro-public-package@{GENEBENCH_PRO_REVISION}"),
        nanocodex_eval::import::Environment::Dockerfile(assets.join("environment")),
        nanocodex_eval::import::Harness::directory(assets.join("verifier"))?,
    ))?)
}

fn import_graphwalks(
    _request: &BenchmarkRequest,
    sources: &SourceStore,
    store: &ImportStore,
    _configuration: Option<&AdapterConfiguration>,
) -> Result<ImportedDataset, AdapterError> {
    let base =
        format!("https://huggingface.co/datasets/openai/graphwalks/resolve/{GRAPHWALKS_REVISION}");
    sources.download(
        "graphwalks/graphwalks_128k_and_shorter.parquet",
        &format!("{base}/graphwalks_128k_and_shorter.parquet"),
        GRAPHWALKS_SHORT_SHA256,
    )?;
    sources.download(
        "graphwalks/graphwalks_256k_to_1mil.parquet",
        &format!("{base}/graphwalks_256k_to_1mil.parquet"),
        GRAPHWALKS_LONG_SHA256,
    )?;
    Ok(store.import(&graphwalks::GraphWalks::new(
        sources.root().join("graphwalks"),
        format!("openai/graphwalks@{GRAPHWALKS_REVISION}"),
        nanocodex_eval::import::Environment::OciImage("python:3.12-slim".to_owned()),
        Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/graphwalks"),
    ))?)
}

fn import_mrcr(
    request: &BenchmarkRequest,
    sources: &SourceStore,
    store: &ImportStore,
    _configuration: Option<&AdapterConfiguration>,
) -> Result<ImportedDataset, AdapterError> {
    let base = format!("https://huggingface.co/datasets/openai/mrcr/resolve/{MRCR_REVISION}");
    for (relative, sha256) in MRCR_FILES {
        sources.download(
            &format!("mrcr/{relative}"),
            &format!("{base}/{relative}"),
            sha256,
        )?;
    }
    let mut importer = mrcr::Mrcr::new(
        sources.root().join("mrcr"),
        format!("openai/mrcr@{MRCR_REVISION}"),
        nanocodex_eval::import::Environment::OciImage("python:3.12-slim".to_owned()),
        Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/mrcr"),
    );
    if !request.all {
        importer = importer.tasks(request.tasks.iter().cloned());
    }
    Ok(store.import(&importer)?)
}

fn import_healthbench_professional(
    _request: &BenchmarkRequest,
    sources: &SourceStore,
    store: &ImportStore,
    _configuration: Option<&AdapterConfiguration>,
) -> Result<ImportedDataset, AdapterError> {
    let dataset = sources.download(
        "healthbench-professional/healthbench_professional_eval.jsonl",
        &format!(
            "https://huggingface.co/datasets/openai/healthbench-professional/resolve/{HEALTHBENCH_PROFESSIONAL_REVISION}/healthbench_professional_eval.jsonl"
        ),
        HEALTHBENCH_PROFESSIONAL_SHA256,
    )?;
    Ok(
        store.import(&healthbench_professional::HealthBenchProfessional::new(
            dataset,
            format!("openai/healthbench-professional@{HEALTHBENCH_PROFESSIONAL_REVISION}"),
            nanocodex_eval::import::Environment::OciImage("python:3.12-slim".to_owned()),
            Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/healthbench-professional"),
        ))?,
    )
}

fn import_gdpval(
    request: &BenchmarkRequest,
    sources: &SourceStore,
    store: &ImportStore,
    _configuration: Option<&AdapterConfiguration>,
) -> Result<ImportedDataset, AdapterError> {
    let checkout = sources.git_checkout_with_materialized_lfs(
        "gdpval",
        "https://huggingface.co/datasets/openai/gdpval.git",
        GDPVAL_REVISION,
    )?;
    let parquet = Path::new(gdpval::PARQUET_PATH);
    let parquet_digest = sources.materialize_checkout_lfs_file(
        "gdpval",
        parquet,
        GDPVAL_REVISION,
        "openai/gdpval",
    )?;
    if parquet_digest != GDPVAL_PARQUET_SHA256 {
        return Err(AdapterError::Source(format!(
            "GDPval Parquet has pinned digest {parquet_digest}, expected {GDPVAL_PARQUET_SHA256}"
        )));
    }
    let selected = (!request.all).then_some(&request.tasks);
    for asset in gdpval::asset_paths(&checkout.join(parquet), selected)? {
        sources.materialize_checkout_lfs_file(
            "gdpval",
            &asset,
            GDPVAL_REVISION,
            "openai/gdpval",
        )?;
    }
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/gdpval");
    let mut importer = gdpval::Gdpval::new(
        checkout,
        format!("openai/gdpval@{GDPVAL_REVISION}"),
        nanocodex_eval::import::Environment::Dockerfile(assets.join("environment")),
        assets.join("verifier"),
    );
    if !request.all {
        importer = importer.tasks(request.tasks.iter().cloned());
    }
    Ok(store.import(&importer)?)
}

fn import_gpqa_diamond(
    _request: &BenchmarkRequest,
    sources: &SourceStore,
    store: &ImportStore,
    _configuration: Option<&AdapterConfiguration>,
) -> Result<ImportedDataset, AdapterError> {
    let checkout = sources.git_checkout(
        "gpqa",
        "https://github.com/idavidrein/gpqa.git",
        GPQA_REVISION,
    )?;
    let archive = checkout.join("dataset.zip");
    sources.validate_file(&archive, GPQA_ZIP_SHA256)?;
    let dataset = sources.extract_zip_member(
        "gpqa-data/gpqa_diamond.csv",
        &archive,
        "dataset/gpqa_diamond.csv",
        "deserted-untie-orchid",
        GPQA_DIAMOND_SHA256,
    )?;
    Ok(store.import(&gpqa_diamond::GpqaDiamond::new(
        dataset,
        format!("idavidrein/gpqa@{GPQA_REVISION}"),
        nanocodex_eval::import::Environment::OciImage("python:3.12-slim".to_owned()),
        Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/gpqa-diamond"),
    ))?)
}

fn import_browsecomp(
    _request: &BenchmarkRequest,
    sources: &SourceStore,
    store: &ImportStore,
    _configuration: Option<&AdapterConfiguration>,
) -> Result<ImportedDataset, AdapterError> {
    let dataset = sources.download(
        "browsecomp/browse_comp_test_set.csv",
        "https://openaipublic.blob.core.windows.net/simple-evals/browse_comp_test_set.csv",
        BROWSECOMP_SHA256,
    )?;
    Ok(store.import(&browsecomp::BrowseComp::new(
        dataset,
        format!("openai/simple-evals@{BROWSECOMP_REVISION}"),
        nanocodex_eval::import::Environment::OciImage("python:3.12-slim".to_owned()),
        Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/browsecomp"),
    ))?)
}

fn import_arc_agi_3(
    _request: &BenchmarkRequest,
    sources: &SourceStore,
    store: &ImportStore,
    _configuration: Option<&AdapterConfiguration>,
) -> Result<ImportedDataset, AdapterError> {
    let benchmarking = sources.git_checkout(
        "arc-agi-3-benchmarking",
        "https://github.com/arcprize/arc-agi-3-benchmarking.git",
        ARC_AGI_3_BENCHMARKING_REVISION,
    )?;
    let toolkit = sources.git_checkout(
        "arc-agi",
        "https://github.com/arcprize/arc-agi.git",
        ARC_AGI_REVISION,
    )?;
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/arc-agi-3");
    Ok(store.import(&arc_agi_3::ArcAgi3::new(
        benchmarking,
        toolkit,
        format!(
            "arcprize/arc-agi-3-benchmarking@{ARC_AGI_3_BENCHMARKING_REVISION}+arcprize/arc-agi@{ARC_AGI_REVISION}"
        ),
        nanocodex_eval::import::Environment::Dockerfile(assets.join("environment")),
        nanocodex_eval::import::Harness::directory(assets.join("verifier"))?,
    ))?)
}

fn import_agents_last_exam(
    _request: &BenchmarkRequest,
    sources: &SourceStore,
    store: &ImportStore,
    _configuration: Option<&AdapterConfiguration>,
) -> Result<ImportedDataset, AdapterError> {
    let source = sources.git_checkout(
        "agents-last-exam",
        "https://github.com/rdi-berkeley/agents-last-exam.git",
        AGENTS_LAST_EXAM_REVISION,
    )?;
    let task_data = sources.prepare_huggingface_archive(
        "agents-last-exam/agents-last-exam-data-archive",
        "ale-tasks-data.tar.gz",
        AGENTS_LAST_EXAM_DATA_REVISION,
        "agents-last-exam-task-data",
    )?;
    Ok(store.import(&agents_last_exam::AgentsLastExam::new(
        source,
        task_data,
        format!(
            "rdi-berkeley/agents-last-exam@{AGENTS_LAST_EXAM_REVISION}+agents-last-exam-data-archive@{AGENTS_LAST_EXAM_DATA_REVISION}"
        ),
        nanocodex_eval::import::Environment::OciImage(AGENTS_LAST_EXAM_IMAGE.to_owned()),
        nanocodex_eval::import::Harness::directory(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/agents-last-exam"),
        )?,
    ))?)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenAiEvalsConfiguration {
    adapter: String,
    registry: PathBuf,
    eval: String,
    revision: String,
    #[serde(default = "default_python_image")]
    image: String,
}

fn import_openai_evals(
    request: &BenchmarkRequest,
    _sources: &SourceStore,
    store: &ImportStore,
    configuration: Option<&AdapterConfiguration>,
) -> Result<ImportedDataset, AdapterError> {
    let configuration =
        configured::<OpenAiEvalsConfiguration>(&request.name, "openai-evals", configuration)?;
    if configuration.recipe.adapter != "openai-evals" {
        return Err(AdapterError::Configuration(format!(
            "benchmark {:?} selected adapter {:?}, expected openai-evals",
            request.name, configuration.recipe.adapter
        )));
    }
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/openai-evals");
    Ok(store.import(&openai_evals::OpenAiEvals::new(
        &request.name,
        resolve_config_path(&configuration.root, &configuration.recipe.registry),
        assets,
        configuration.recipe.eval,
        configuration.recipe.revision,
        nanocodex_eval::import::Environment::OciImage(configuration.recipe.image),
    ))?)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExternalConfiguration {
    adapter: String,
    manifest: PathBuf,
}

fn import_external(
    request: &BenchmarkRequest,
    _sources: &SourceStore,
    store: &ImportStore,
    configuration: Option<&AdapterConfiguration>,
) -> Result<ImportedDataset, AdapterError> {
    let configuration =
        configured::<ExternalConfiguration>(&request.name, "external", configuration)?;
    if configuration.recipe.adapter != "external" {
        return Err(AdapterError::Configuration(format!(
            "benchmark {:?} selected adapter {:?}, expected external",
            request.name, configuration.recipe.adapter
        )));
    }
    Ok(
        store.import(&external::ExternalHarness::new(resolve_config_path(
            &configuration.root,
            &configuration.recipe.manifest,
        )))?,
    )
}

struct Configured<T> {
    root: PathBuf,
    recipe: T,
}

fn configured<T: DeserializeOwned>(
    benchmark: &str,
    expected_adapter: &str,
    configuration: Option<&AdapterConfiguration>,
) -> Result<Configured<T>, AdapterError> {
    let configuration = configuration.ok_or_else(|| {
        AdapterError::Configuration(format!(
            "benchmark {benchmark:?} requires [{expected_adapter}] configuration"
        ))
    })?;
    let recipe = configuration
        .value
        .clone()
        .try_into::<T>()
        .map_err(|error| AdapterError::Configuration(error.to_string()))?;
    Ok(Configured {
        root: configuration.root.clone(),
        recipe,
    })
}

fn resolve_config_path(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

fn default_python_image() -> String {
    "python:3.12-slim".to_owned()
}

impl AdapterCatalog {
    /// Uses `imports/` and `sources/` below one durable evaluator state root.
    #[must_use]
    pub fn new(state_directory: impl AsRef<Path>) -> Self {
        let root = state_directory.as_ref();
        Self {
            imports: root.join("imports"),
            sources: root.join("sources"),
        }
    }

    /// Acquires, imports, and selects every configured benchmark task.
    pub async fn resolve(
        &self,
        config: impl AsRef<Path>,
        selectors: &[String],
    ) -> Result<Vec<ResolvedTask>, AdapterError> {
        let config = config.as_ref();
        let text = fs::read_to_string(config).map_err(|error| {
            AdapterError::Configuration(format!("failed to read {}: {error}", config.display()))
        })?;
        let manifest: AdapterManifest = toml::from_str(&text).map_err(|error| {
            AdapterError::Configuration(format!("failed to parse {}: {error}", config.display()))
        })?;
        let config = config.canonicalize().map_err(|error| {
            AdapterError::Configuration(format!("failed to resolve {}: {error}", config.display()))
        })?;
        let root = config
            .parent()
            .ok_or_else(|| AdapterError::Configuration("config has no parent".to_owned()))?;
        let requests = parse_requests(selectors)?;
        let mut jobs = tokio::task::JoinSet::new();
        for request in requests.into_values() {
            let (adapter, configuration) = installed_adapter(&request.name, &manifest, root)?;
            let imports = self.imports.clone();
            let sources = self.sources.clone();
            jobs.spawn_blocking(move || {
                let store = ImportStore::new(imports);
                let sources = SourceStore::new(sources);
                let dataset = (adapter.import)(&request, &sources, &store, configuration.as_ref())?;
                select_tasks(&request, &dataset, adapter.matches)
            });
        }
        let mut selected = Vec::new();
        while let Some(result) = jobs.join_next().await {
            selected.extend(result.map_err(|error| AdapterError::Worker(error.to_string()))??);
        }
        selected.sort_by(|left, right| left.selector.cmp(&right.selector));
        Ok(selected)
    }
}

fn installed_adapter(
    name: &str,
    manifest: &AdapterManifest,
    root: &Path,
) -> Result<(InstalledAdapter, Option<AdapterConfiguration>), AdapterError> {
    if let Some(adapter) = INSTALLED_ADAPTERS
        .iter()
        .copied()
        .find(|adapter| adapter.names.contains(&name))
    {
        return Ok((adapter, None));
    }
    let value = manifest
        .benchmark
        .get(name)
        .ok_or_else(|| AdapterError::UnknownBenchmark(name.to_owned()))?;
    let kind = value
        .get("adapter")
        .and_then(toml::Value::as_str)
        .ok_or_else(|| {
            AdapterError::Configuration(format!("benchmark {name:?} must declare a string adapter"))
        })?;
    let adapter = INSTALLED_ADAPTERS
        .iter()
        .copied()
        .find(|adapter| adapter.kind == kind)
        .ok_or_else(|| AdapterError::UnknownBenchmark(format!("{name} (adapter {kind})")))?;
    Ok((
        adapter,
        Some(AdapterConfiguration {
            root: root.to_path_buf(),
            value: value.clone(),
        }),
    ))
}

fn parse_requests(
    selectors: &[String],
) -> Result<BTreeMap<String, BenchmarkRequest>, AdapterError> {
    let mut unique = BTreeSet::new();
    let mut requests = BTreeMap::<String, BenchmarkRequest>::new();
    for selector in selectors {
        if !unique.insert(selector) {
            return Err(AdapterError::DuplicateSelector(selector.clone()));
        }
        let (benchmark, task) = selector
            .split_once('/')
            .filter(|(benchmark, task)| !benchmark.is_empty() && !task.is_empty())
            .ok_or_else(|| AdapterError::InvalidSelector(selector.clone()))?;
        let request = requests
            .entry(benchmark.to_owned())
            .or_insert_with(|| BenchmarkRequest {
                name: benchmark.to_owned(),
                all: false,
                tasks: BTreeSet::new(),
            });
        if task == "*" {
            request.all = true;
        } else {
            request.tasks.insert(task.to_owned());
        }
    }
    for request in requests.values() {
        if request.all && !request.tasks.is_empty() {
            return Err(AdapterError::InvalidSelector(format!(
                "{} mixes * with explicit tasks",
                request.name
            )));
        }
    }
    Ok(requests)
}

fn select_tasks(
    request: &BenchmarkRequest,
    dataset: &ImportedDataset,
    matches: fn(&str, &str) -> bool,
) -> Result<Vec<ResolvedTask>, AdapterError> {
    let mut missing = request.tasks.clone();
    let mut selected = Vec::new();
    for task in dataset.tasks() {
        let selected_name = request
            .tasks
            .iter()
            .find(|selected| matches(selected, task.name()));
        if request.all || selected_name.is_some() {
            if let Some(name) = selected_name {
                missing.remove(name);
            }
            selected.push(ResolvedTask {
                selector: format!(
                    "{}/{}",
                    request.name,
                    selected_name.map_or_else(|| task.name(), String::as_str)
                ),
                task: task.clone(),
            });
        }
    }
    if !missing.is_empty() {
        return Err(AdapterError::MissingTasks {
            benchmark: request.name.clone(),
            tasks: missing.into_iter().collect::<Vec<_>>().join(", "),
        });
    }
    Ok(selected)
}

#[allow(dead_code)]
pub(crate) fn exact_task(selected: &str, normalized: &str) -> bool {
    selected == normalized
}

impl From<source::SourceError> for AdapterError {
    fn from(error: source::SourceError) -> Self {
        Self::Source(error.to_string())
    }
}

#[allow(dead_code)]
fn sha256_values(values: impl IntoIterator<Item = impl AsRef<[u8]>>) -> String {
    let mut digest = Sha256::new();
    for value in values {
        digest.update(Sha256::digest(value));
    }
    hex::encode(digest.finalize())
}

#[allow(dead_code)]
fn safe_case_id(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut separator = false;
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'+') {
            output.push(char::from(byte));
            separator = false;
        } else if !separator && !output.is_empty() {
            output.push('-');
            separator = true;
        }
    }
    while output.ends_with('-') {
        output.pop();
    }
    if output.is_empty() || output == "." || output == ".." {
        let digest = Sha256::digest(value.as_bytes());
        format!("case-{}", &hex::encode(digest)[..16])
    } else {
        output
    }
}

fn sha256_file(path: &Path) -> Result<String, ImportError> {
    let file = fs::File::open(path).map_err(|source| ImportError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut reader = BufReader::new(file);
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer).map_err(|source| ImportError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(hex::encode(digest.finalize()))
}

fn read_json_lines<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Vec<T>, ImportError> {
    let text = fs::read_to_string(path).map_err(|source| ImportError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(line, value)| {
            serde_json::from_str(value).map_err(|source| {
                ImportError::Invalid(format!(
                    "failed to decode {} line {}: {source}",
                    path.display(),
                    line + 1
                ))
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_exact_and_all_task_selections() {
        let requests =
            parse_requests(&["one/a".to_owned(), "one/b".to_owned(), "two/*".to_owned()]).unwrap();

        assert_eq!(requests["one"].tasks.len(), 2);
        assert!(!requests["one"].all);
        assert!(requests["two"].all);
    }

    #[test]
    fn rejects_ambiguous_all_selection() {
        let error = parse_requests(&["one/*".to_owned(), "one/a".to_owned()]).unwrap_err();

        assert!(error.to_string().contains("mixes *"));
    }

    #[test]
    fn harbor_selectors_hide_upstream_name_prefixes() {
        assert!(matches_harbor_task("fix-git", "terminal-bench/fix-git"));
        assert!(matches_harbor_task(
            "aiomonitor-task-snapshots-diff",
            "datacurve/aiomonitor-task-snapshots-diff"
        ));
    }
}
