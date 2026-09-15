use std::{hint::black_box, path::PathBuf, time::Duration};

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use nanocodex_vm::{
    host::{BlockDevice, EgressLease, GuestCommand, VmConfig},
    tools::{GuestRuntimeDisk, GuestRuntimeDiskError, VmCommand, VmToolSession},
};
use tokio::process::Command;

const RUNTIME_DEVICE: &str = "/dev/vdb";
const RUNTIME_MOUNT: &str = "/run/nanocodex";
#[cfg(target_os = "linux")]
const FIRMWARE_LIBRARY_PATH_ENVIRONMENT: &str = "LD_LIBRARY_PATH";
#[cfg(target_os = "macos")]
const FIRMWARE_LIBRARY_PATH_ENVIRONMENT: &str = "DYLD_LIBRARY_PATH";

fn protocol_server() -> Command {
    let script = r#"
while IFS= read -r request; do
  id=${request#*\"id\":}
  id=${id%%,*}
  id=${id%%\}*}
  case "$request" in
    *'"kind":"ready"'*)
      printf '{"kind":"ready","payload":{"id":%s,"error":null}}\n' "$id"
      ;;
    *'"kind":"execute"'*)
      printf '{"kind":"execute","payload":{"id":%s,"exit_code":0,"stdout":"","stderr":"","error":null,"timed_out":false,"output_limit_exceeded":false}}\n' "$id"
      ;;
    *'"kind":"shutdown"'*)
      printf '{"kind":"shutdown","payload":{"id":%s,"error":null}}\n' "$id"
      exit 0
      ;;
    *)
      exit 9
      ;;
  esac
done
"#;
    let mut command = Command::new("/bin/sh");
    command.arg("-c").arg(script);
    command
}

fn benchmark_guest_runtime_cache(criterion: &mut Criterion) {
    let directory = tempfile::tempdir().expect("runtime cache fixture");
    let binary = directory.path().join("nanocodex-vm-guest");
    let cache = directory.path().join("cache");
    std::fs::write(&binary, b"\x7fELF benchmark guest runtime").expect("benchmark guest runtime");
    GuestRuntimeDisk::prepare(&binary, &cache).expect("prime guest runtime cache");

    let mut group = criterion.benchmark_group("vm_guest_runtime_cache");
    group.measurement_time(Duration::from_secs(2));
    group.bench_function("warm_prepare", |bencher| {
        bencher.iter(|| {
            black_box(
                GuestRuntimeDisk::prepare(black_box(&binary), black_box(&cache))
                    .expect("warm guest runtime cache"),
            );
        });
    });
    group.finish();
}

fn benchmark_protocol(criterion: &mut Criterion) {
    let runtime = benchmark_runtime();
    let session = {
        let _runtime = runtime.enter();
        VmToolSession::spawn(&mut protocol_server()).expect("protocol session")
    };
    let mut group = criterion.benchmark_group("vm_session_protocol");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(2));
    group.bench_function("retained_command_rpc", |bencher| {
        bencher.to_async(&runtime).iter(|| async {
            let output = session
                .command(VmCommand::new("/bin/true"))
                .await
                .expect("protocol command");
            black_box(output);
        });
    });
    group.bench_function("spawn_first_rpc_shutdown", |bencher| {
        bencher.to_async(&runtime).iter_batched(
            || VmToolSession::spawn(&mut protocol_server()).expect("protocol session"),
            |session| async move {
                let output = session
                    .command(VmCommand::new("/bin/true"))
                    .await
                    .expect("protocol command");
                session.shutdown().await.expect("protocol shutdown");
                black_box(output);
            },
            BatchSize::PerIteration,
        );
    });
    group.finish();
    runtime
        .block_on(session.shutdown())
        .expect("protocol session shutdown");
}

struct LiveVm {
    vmm: PathBuf,
    rootfs: PathBuf,
    runtime: PathBuf,
    firmware: PathBuf,
    _runtime_directory: Option<tempfile::TempDir>,
    _runtime_disk: Option<GuestRuntimeDisk>,
}

impl LiveVm {
    fn from_environment() -> Option<Self> {
        let runtime = PathBuf::from(std::env::var_os("NANOCODEX_VM_RUNTIME")?);
        let (runtime_directory, runtime_disk, runtime) = prepare_runtime_disk(runtime);
        Some(Self {
            vmm: std::env::var_os("NANOCODEX_VM_VMM")?.into(),
            rootfs: std::env::var_os("NANOCODEX_VM_ROOTFS")?.into(),
            runtime,
            firmware: std::env::var_os("NANOCODEX_VM_FIRMWARE")?.into(),
            _runtime_directory: runtime_directory,
            _runtime_disk: runtime_disk,
        })
    }

    fn private_root(&self) -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().expect("private VM directory");
        let root = directory.path().join("rootfs.ext4");
        reflink_copy::reflink(&self.rootfs, &root).expect("rootfs copy-on-write clone");
        (directory, root)
    }

    async fn spawn(&self, root: PathBuf) -> VmToolSession {
        let config = VmConfig::ext4(root)
            .cpus(2)
            .memory_mib(768)
            .block_device(BlockDevice::read_only("nanocodex-runtime", &self.runtime));
        let init = format!(
            "set -eu; mkdir -p $1 {RUNTIME_MOUNT}; \
             mount -t ext4 -o ro {RUNTIME_DEVICE} {RUNTIME_MOUNT}; \
             exec {RUNTIME_MOUNT}/nanocodex-vm-guest $1"
        );
        let guest = GuestCommand::new("/bin/sh")
            .arg("-c")
            .arg(init)
            .arg("nanocodex-vm-init")
            .arg("/workspace");
        let mut vmm = Command::new(&self.vmm);
        vmm.arg("--vmm")
            .env_clear()
            .env(FIRMWARE_LIBRARY_PATH_ENVIRONMENT, &self.firmware);
        VmToolSession::spawn_configured(vmm, config, guest, EgressLease::disabled())
            .await
            .expect("live VM session")
    }

    async fn run_once(&self, root: PathBuf) {
        let session = self.spawn(root).await;
        let output = session
            .command(VmCommand::new("/bin/true"))
            .await
            .expect("live VM command");
        assert_eq!(output.exit_code, 0);
        session.shutdown().await.expect("live VM shutdown");
    }
}

fn prepare_runtime_disk(
    runtime: PathBuf,
) -> (Option<tempfile::TempDir>, Option<GuestRuntimeDisk>, PathBuf) {
    let directory = tempfile::tempdir().expect("runtime disk directory");
    match GuestRuntimeDisk::prepare(&runtime, directory.path()) {
        Ok(disk) => {
            let path = disk.path().to_owned();
            (Some(directory), Some(disk), path)
        }
        Err(GuestRuntimeDiskError::NotElf(_)) => (None, None, runtime),
        Err(error) => panic!("guest runtime disk: {error}"),
    }
}

fn benchmark_live_vm(criterion: &mut Criterion) {
    let Some(vm) = LiveVm::from_environment() else {
        eprintln!(
            "live VM benchmark skipped; set NANOCODEX_VM_VMM, NANOCODEX_VM_ROOTFS, \
             NANOCODEX_VM_RUNTIME, and NANOCODEX_VM_FIRMWARE"
        );
        return;
    };
    let runtime = benchmark_runtime();
    let (retained_directory, retained_root) = vm.private_root();
    let retained = runtime.block_on(vm.spawn(retained_root));
    let mut group = criterion.benchmark_group("vm_session_live");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(10));
    group.bench_function("retained_command_rpc", |bencher| {
        bencher.to_async(&runtime).iter(|| async {
            let output = retained
                .command(VmCommand::new("/bin/true"))
                .await
                .expect("live VM command");
            black_box(output);
        });
    });
    let vm = &vm;
    group.bench_function("boot_first_rpc_shutdown", |bencher| {
        bencher.to_async(&runtime).iter_batched(
            || vm.private_root(),
            |(directory, root)| async move {
                let _directory = directory;
                vm.run_once(root).await;
            },
            BatchSize::PerIteration,
        );
    });
    group.finish();
    runtime
        .block_on(retained.shutdown())
        .expect("retained live VM shutdown");
    drop(retained_directory);
}

fn benchmark_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("benchmark runtime")
}

criterion_group!(
    benches,
    benchmark_guest_runtime_cache,
    benchmark_protocol,
    benchmark_live_vm
);
criterion_main!(benches);
