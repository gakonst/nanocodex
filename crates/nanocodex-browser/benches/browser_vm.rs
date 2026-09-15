#![cfg(any(
    all(target_os = "linux", not(target_env = "musl")),
    all(target_os = "macos", target_arch = "aarch64")
))]

use std::{ffi::OsString, hint::black_box, path::PathBuf, time::Duration};

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use nanocodex_browser::{
    BrowserAction, BrowserTarget,
    vm::{BrowserVm, BrowserVmBuilder},
};

const BENCHMARK_PAGE: &str =
    "data:text/html,<main><h1 id='status'>Ready</h1><button>Save</button></main>";

#[derive(Clone)]
struct LiveBrowserVm {
    rootfs: PathBuf,
    vmm: PathBuf,
    gvproxy: PathBuf,
    firmware: Option<PathBuf>,
    vmm_arguments: Vec<OsString>,
}

impl LiveBrowserVm {
    fn from_environment() -> Option<Self> {
        let vmm_arguments = std::env::var_os("NANOCODEX_BROWSER_VM_VMM_ARGS")
            .map(|arguments| serde_json::from_str::<Vec<String>>(&arguments.to_string_lossy()))
            .transpose()
            .ok()?
            .unwrap_or_default()
            .into_iter()
            .map(OsString::from)
            .collect();
        Some(Self {
            rootfs: std::env::var_os("NANOCODEX_BROWSER_VM_ROOTFS")?.into(),
            vmm: std::env::var_os("NANOCODEX_BROWSER_VM_VMM")?.into(),
            gvproxy: std::env::var_os("NANOCODEX_BROWSER_VM_GVPROXY")?.into(),
            firmware: std::env::var_os("NANOCODEX_BROWSER_VM_FIRMWARE").map(Into::into),
            vmm_arguments,
        })
    }

    fn builder(&self) -> BrowserVmBuilder {
        let mut builder = BrowserVm::builder(&self.rootfs, &self.vmm, &self.gvproxy)
            .vmm_args(self.vmm_arguments.clone());
        if let Some(firmware) = &self.firmware {
            builder = builder.firmware_directory(firmware);
        }
        builder
    }

    async fn spawn_open(&self) -> BrowserVm {
        let vm = self.builder().spawn().await.expect("spawn browser VM");
        vm.browser()
            .execute(BrowserAction::Open {
                url: BENCHMARK_PAGE.to_owned(),
            })
            .await
            .expect("open benchmark page");
        vm
    }
}

fn benchmark_live_vm(criterion: &mut Criterion) {
    let Some(config) = LiveBrowserVm::from_environment() else {
        eprintln!(
            "live browser VM benchmark skipped; set NANOCODEX_BROWSER_VM_ROOTFS, \
             NANOCODEX_BROWSER_VM_VMM, and NANOCODEX_BROWSER_VM_GVPROXY"
        );
        return;
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("benchmark runtime");
    let retained = runtime.block_on(config.spawn_open());
    let mut group = criterion.benchmark_group("browser_vm_live");
    group.sample_size(10);
    // A browser session deliberately retains at most 128 visual artifacts.
    // Keep the warm window bounded so screenshot sampling measures the normal
    // public path without exhausting session policy.
    group.warm_up_time(Duration::from_millis(250));
    group.measurement_time(Duration::from_millis(500));
    group.bench_function("warm_get_text", |bencher| {
        bencher.to_async(&runtime).iter(|| async {
            let result = retained
                .browser()
                .execute(BrowserAction::GetText {
                    target: BrowserTarget::css("#status"),
                })
                .await
                .expect("warm text action");
            black_box(result);
        });
    });
    group.bench_function("warm_semantic_snapshot", |bencher| {
        bencher.to_async(&runtime).iter(|| async {
            let result = retained
                .browser()
                .execute(BrowserAction::Snapshot {
                    interactive: true,
                    compact: true,
                    depth: None,
                    selector: None,
                    include_urls: false,
                })
                .await
                .expect("warm snapshot");
            black_box(result);
        });
    });
    group.bench_function("warm_screenshot", |bencher| {
        bencher.to_async(&runtime).iter(|| async {
            let result = retained
                .browser()
                .execute(BrowserAction::Screenshot {
                    full_page: false,
                    annotate: false,
                    target: None,
                })
                .await
                .expect("warm screenshot");
            black_box(result);
        });
    });
    let config = &config;
    group.bench_function("boot_first_action_shutdown", |bencher| {
        bencher.to_async(&runtime).iter_batched(
            || config.clone(),
            |config| async move {
                let vm = config.spawn_open().await;
                vm.shutdown().await.expect("shutdown browser VM");
            },
            BatchSize::PerIteration,
        );
    });
    group.finish();
    runtime
        .block_on(retained.shutdown())
        .expect("shutdown retained browser VM");
}

criterion_group!(benches, benchmark_live_vm);
criterion_main!(benches);
