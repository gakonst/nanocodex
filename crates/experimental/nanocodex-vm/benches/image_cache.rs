use std::{
    fs::{self, File},
    hint::black_box,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use flate2::{Compression, write::GzEncoder};
use nanocodex_vm::image::{CachePolicy, VmImageBuilder};
use serde::Serialize;
use sha2::{Digest, Sha256};

const FIXTURE_IMAGE: &str = "example.invalid/nanocodex-vm-benchmark:latest";
const FIXTURE_MANIFEST: &str =
    "sha256:2eeb0b07339f47ea087a4a9a3ece22c2fd80cc74a812870f163189812f9fc4df";
const FIXTURE_LAYER: &str =
    "sha256:018f4abf7d81ee0c83a4a0ef7fd0f2e3ea315714209860653c4af66a648824cb";
const DISK_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Serialize)]
struct ReferenceRecord<'a> {
    version: u32,
    image_reference: &'a str,
    manifest_digest: &'a str,
    layers: [LayerRecord<'a>; 1],
    config: ImageConfig,
}

#[derive(Serialize)]
struct LayerRecord<'a> {
    digest: &'a str,
    media_type: &'a str,
}

#[derive(Default, Serialize)]
struct ImageConfig {
    environment: std::collections::BTreeMap<String, String>,
    working_directory: String,
}

struct Fixture {
    _root: tempfile::TempDir,
    context: PathBuf,
    cache: PathBuf,
    attempts: PathBuf,
    builder: VmImageBuilder,
}

impl Fixture {
    fn new(runtime: &tokio::runtime::Runtime) -> Self {
        let root = tempfile::tempdir().expect("benchmark tempdir");
        let context = root.path().join("context");
        let cache = root.path().join("cache");
        let attempts = root.path().join("attempts");
        fs::create_dir_all(&context).expect("context directory");
        fs::create_dir_all(&attempts).expect("attempt directory");
        fs::write(
            context.join("Dockerfile"),
            format!("FROM {FIXTURE_IMAGE}\nWORKDIR /workspace\n"),
        )
        .expect("Dockerfile");

        let blobs = cache.join("blobs");
        fs::create_dir_all(&blobs).expect("blob cache");
        write_shell_layer(&blobs.join(FIXTURE_LAYER.replace(':', "-")));
        let reference = ReferenceRecord {
            version: 2,
            image_reference: FIXTURE_IMAGE,
            manifest_digest: FIXTURE_MANIFEST,
            layers: [LayerRecord {
                digest: FIXTURE_LAYER,
                media_type: "application/vnd.oci.image.layer.v1.tar+gzip",
            }],
            config: ImageConfig::default(),
        };
        let references = cache.join("references");
        fs::create_dir_all(&references).expect("reference cache");
        fs::write(
            references.join(format!("{}.json", reference_key(FIXTURE_IMAGE))),
            serde_json::to_vec(&reference).expect("reference JSON"),
        )
        .expect("reference record");

        let builder = VmImageBuilder::new(
            root.path().join("unused-vmm"),
            root.path().join("unused-runtime.ext4"),
        );
        runtime
            .block_on(builder.prepare(&context, DISK_BYTES, &cache, CachePolicy::Reuse))
            .expect("prime immutable image cache");
        Self {
            _root: root,
            context,
            cache,
            attempts,
            builder,
        }
    }
}

fn write_shell_layer(path: &Path) {
    let output = File::create(path).expect("layer");
    let encoder = GzEncoder::new(output, Compression::fast());
    let mut archive = tar::Builder::new(encoder);

    let mut directory = tar::Header::new_gnu();
    directory.set_entry_type(tar::EntryType::Directory);
    directory.set_mode(0o755);
    directory.set_size(0);
    directory.set_cksum();
    archive
        .append_data(&mut directory, "bin/", std::io::empty())
        .expect("bin directory");

    let contents = b"#!/bin/sh\n";
    let mut shell = tar::Header::new_gnu();
    shell.set_entry_type(tar::EntryType::Regular);
    shell.set_mode(0o755);
    shell.set_size(contents.len() as u64);
    shell.set_cksum();
    archive
        .append_data(&mut shell, "bin/sh", contents.as_slice())
        .expect("shell");
    let encoder = archive.into_inner().expect("finish tar");
    drop(encoder.finish().expect("finish gzip"));
}

fn reference_key(image: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"nanocodex-vm-reference-v1\0linux\0");
    #[cfg(target_arch = "aarch64")]
    hasher.update(b"arm64");
    #[cfg(target_arch = "x86_64")]
    hasher.update(b"amd64");
    hasher.update([0]);
    hasher.update(image.as_bytes());
    hex::encode(hasher.finalize())
}

fn benchmark_image_cache(criterion: &mut Criterion) {
    let runtime = tokio::runtime::Runtime::new().expect("benchmark runtime");
    let fixture = Fixture::new(&runtime);
    let counter = AtomicU64::new(0);

    let mut group = criterion.benchmark_group("vm_image_cache");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(2));
    group.bench_function("warm_prepare", |bencher| {
        bencher.to_async(&runtime).iter(|| async {
            let prepared = fixture
                .builder
                .prepare(
                    &fixture.context,
                    DISK_BYTES,
                    &fixture.cache,
                    CachePolicy::Reuse,
                )
                .await
                .expect("warm preparation");
            black_box((prepared.path(), prepared.workdir(), prepared.environment()));
        });
    });
    group.bench_function("attempt_reflink", |bencher| {
        let prepared = runtime
            .block_on(fixture.builder.prepare(
                &fixture.context,
                DISK_BYTES,
                &fixture.cache,
                CachePolicy::Reuse,
            ))
            .expect("prepared image");
        bencher.iter_batched(
            || {
                fixture.attempts.join(format!(
                    "attempt-{}.ext4",
                    counter.fetch_add(1, Ordering::Relaxed)
                ))
            },
            |destination| {
                let bytes = prepared.reflink_to(&destination).expect("attempt reflink");
                fs::remove_file(destination).expect("remove attempt");
                black_box(bytes);
            },
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

criterion_group!(benches, benchmark_image_cache);
criterion_main!(benches);
