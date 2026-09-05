use std::{
    fs,
    io::{self, Read as _},
    path::{Path, PathBuf},
};

use arcbox_ext4::{
    Formatter, Reader,
    constants::{file_mode, make_mode},
};
use nanocodex_vm::{
    host::{
        BlockDevice, EgressLease, Network, VmConfig, create_sparse_overlay_disk,
        overlay_guest_command,
    },
    image::{CachePolicy, DiskStatus, VmImageBuilder},
    tools::{GuestRuntimeDisk, VmCommand, VmToolSession},
};
use sha2::{Digest as _, Sha256};
use tokio::process::Command;

const DISK_BYTES: u64 = 512 * 1024 * 1024;

#[tokio::test]
#[ignore = "requires a signed libkrun VMM, firmware, current guest ELF, and OCI cache"]
async fn run_instruction_uses_the_public_private_config_vmm_contract() {
    let vmm = required_path("NANOCODEX_VM_IMAGE_VMM");
    let guest = required_path("NANOCODEX_VM_IMAGE_RUNTIME");
    let firmware = required_path("NANOCODEX_VM_IMAGE_FIRMWARE");
    let cache = required_path("NANOCODEX_VM_IMAGE_CACHE");
    let runtime =
        GuestRuntimeDisk::prepare(guest, &cache).expect("content-addressed guest runtime disk");
    let context = tempfile::tempdir().expect("build context");
    fs::write(
        context.path().join("Dockerfile"),
        concat!(
            "FROM alpine:3.24\n",
            "RUN printf nanocodex-vm-image-live > /nanocodex-vm-image-proof && ",
            "printf %s \"$NANOCODEX_BUILD_EGRESS_PROOF\" > /nanocodex-vm-egress-proof\n",
            "WORKDIR /workspace\n",
        ),
    )
    .expect("Dockerfile");

    let mut egress = EgressLease::internet();
    egress
        .insert_environment("NANOCODEX_BUILD_EGRESS_PROOF", "inherited-by-run")
        .expect("build egress environment");
    egress
        .set_build_cache_scope("image-live-build-egress-v1")
        .expect("build egress cache scope");
    let builder = VmImageBuilder::new(vmm, runtime.path())
        .firmware_directory(firmware)
        .egress(egress)
        .vmm_arg("--vmm");
    let image = builder
        .prepare(context.path(), DISK_BYTES, &cache, CachePolicy::Reuse)
        .await
        .expect("prepared image");

    let mut disk = Reader::new(image.path()).expect("prepared ext4");
    assert_eq!(
        disk.read_file("/nanocodex-vm-image-proof", 0, Some(64))
            .expect("proof file"),
        b"nanocodex-vm-image-live"
    );
    assert_eq!(
        disk.read_file("/nanocodex-vm-egress-proof", 0, Some(64))
            .expect("egress proof file"),
        b"inherited-by-run"
    );
    assert!(
        !disk.exists("/run/nanocodex-build-resolver"),
        "build-only resolver state must not persist in the prepared image"
    );
    assert_eq!(image.workdir(), "/workspace");

    let warm = builder
        .prepare(context.path(), DISK_BYTES, &cache, CachePolicy::Reuse)
        .await
        .expect("warm prepared image");
    assert_eq!(warm.disk_status(), DiskStatus::Hit);
    assert_eq!(warm.path(), image.path());
}

#[tokio::test]
#[ignore = "requires a signed libkrun VMM, firmware, current guest ELF, and OCI cache"]
async fn guest_overlay_resets_to_an_immutable_base_without_host_reflinks() {
    let vmm = required_path("NANOCODEX_VM_IMAGE_VMM");
    let guest = required_path("NANOCODEX_VM_IMAGE_RUNTIME");
    let firmware = required_path("NANOCODEX_VM_IMAGE_FIRMWARE");
    let cache = required_path("NANOCODEX_VM_IMAGE_CACHE");
    let runtime =
        GuestRuntimeDisk::prepare(guest, &cache).expect("content-addressed guest runtime disk");
    let context = tempfile::tempdir().expect("build context");
    fs::write(
        context.path().join("Dockerfile"),
        concat!(
            "FROM alpine:3.24\n",
            "RUN printf immutable-base > /nanocodex-overlay-base\n",
            "WORKDIR /workspace\n",
        ),
    )
    .expect("Dockerfile");
    let builder = VmImageBuilder::new(&vmm, runtime.path())
        .firmware_directory(&firmware)
        .vmm_arg("vm-run-config")
        .vmm_arg("--config");
    let image = builder
        .prepare(context.path(), DISK_BYTES, &cache, CachePolicy::Reuse)
        .await
        .expect("prepared image");
    let base_digest = file_digest(image.path()).expect("base digest before attempts");
    let attempts = tempfile::tempdir().expect("attempt disks");
    let additional = attempts.path().join("additional.ext4");
    format_probe_disk(&additional).expect("additional block disk");

    let first_upper = attempts.path().join("first-upper.ext4");
    create_sparse_overlay_disk(&first_upper, DISK_BYTES).expect("first sparse upper disk");
    let first = spawn_overlay(
        &vmm,
        &firmware,
        runtime.path(),
        image.path(),
        &first_upper,
        Some(&additional),
    )
    .await;
    let first_output = first
        .command(VmCommand::new("/bin/sh").arg("-c").arg(
            "set -eu; grep -qw overlay /proc/filesystems; \
                 test \"$(cat /nanocodex-overlay-base)\" = immutable-base; \
                 mkdir -p /mnt/nanocodex-additional; \
                 mount -t ext4 -o ro /dev/vdd /mnt/nanocodex-additional; \
                 test \"$(cat /mnt/nanocodex-additional/proof)\" = fourth-device; \
                 printf attempt-one > /workspace/attempt-marker; \
                 printf changed > /etc/nanocodex-overlay-proof; \
                 rm /nanocodex-overlay-base",
        ))
        .await
        .expect("first overlay command");
    assert_eq!(first_output.exit_code, 0, "{:?}", first_output.stderr);
    first.shutdown().await.expect("first overlay shutdown");

    assert_eq!(
        file_digest(image.path()).expect("base digest after first attempt"),
        base_digest
    );
    let mut base = Reader::new(image.path()).expect("immutable base reader");
    assert_eq!(
        base.read_file("/nanocodex-overlay-base", 0, None)
            .expect("base marker"),
        b"immutable-base"
    );
    assert!(!base.exists("/workspace/attempt-marker"));
    assert!(!base.exists("/etc/nanocodex-overlay-proof"));

    let second_upper = attempts.path().join("second-upper.ext4");
    create_sparse_overlay_disk(&second_upper, DISK_BYTES).expect("second sparse upper disk");
    let second = spawn_overlay(
        &vmm,
        &firmware,
        runtime.path(),
        image.path(),
        &second_upper,
        None,
    )
    .await;
    let second_output = second
        .command(VmCommand::new("/bin/sh").arg("-c").arg(
            "set -eu; test -f /nanocodex-overlay-base; \
             test ! -e /workspace/attempt-marker; \
             test ! -e /etc/nanocodex-overlay-proof",
        ))
        .await
        .expect("second overlay command");
    assert_eq!(second_output.exit_code, 0, "{:?}", second_output.stderr);
    second.shutdown().await.expect("second overlay shutdown");
    assert_eq!(
        file_digest(image.path()).expect("base digest after second attempt"),
        base_digest
    );
}

async fn spawn_overlay(
    vmm: &Path,
    firmware: &Path,
    runtime: &Path,
    lower: &Path,
    upper: &Path,
    additional: Option<&Path>,
) -> VmToolSession {
    let mut config = VmConfig::overlay_ext4(runtime, lower, upper)
        .cpus(2)
        .memory_mib(768)
        .network(Network::Disabled);
    if let Some(additional) = additional {
        config = config.block_device(BlockDevice::read_only("additional", additional));
    }
    let guest = overlay_guest_command("/workspace", "");
    let mut command = Command::new(vmm);
    command.args(["vm-run-config", "--config"]);
    #[cfg(target_os = "linux")]
    command.env("LD_LIBRARY_PATH", firmware);
    #[cfg(target_os = "macos")]
    command.env("DYLD_LIBRARY_PATH", firmware);
    VmToolSession::spawn_configured(command, config, guest, EgressLease::disabled())
        .await
        .expect("overlay VM session")
}

fn format_probe_disk(path: &Path) -> Result<(), arcbox_ext4::error::FormatError> {
    let mut formatter = Formatter::new(path, 4_096, 128 * 1024 * 1024)?;
    let mut proof = b"fourth-device".as_slice();
    formatter.create(
        "/proof",
        make_mode(file_mode::S_IFREG, 0o444),
        None,
        None,
        Some(&mut proof),
        Some(0),
        Some(0),
        None,
    )?;
    formatter.close()
}

fn file_digest(path: &Path) -> io::Result<[u8; 32]> {
    let mut file = fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let bytes = file.read(&mut buffer)?;
        if bytes == 0 {
            break;
        }
        digest.update(&buffer[..bytes]);
    }
    Ok(digest.finalize().into())
}

fn required_path(name: &str) -> PathBuf {
    std::env::var_os(name).map_or_else(
        || panic!("{name} must name an existing live-test input"),
        PathBuf::from,
    )
}
