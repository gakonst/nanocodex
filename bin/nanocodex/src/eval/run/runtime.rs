use std::{
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

use eyre::{Result, WrapErr as _, eyre};
use nanocodex_vm::tools::{GuestRuntimeDisk, GuestRuntimeDiskStatus};
use tracing::info;

#[cfg(target_arch = "aarch64")]
const VM_GUEST_TARGET: &str = "aarch64-unknown-linux-musl";
#[cfg(target_arch = "x86_64")]
const VM_GUEST_TARGET: &str = "x86_64-unknown-linux-musl";
#[cfg(target_arch = "aarch64")]
const VM_GUEST_ELF_MACHINE: u16 = 183;
#[cfg(target_arch = "x86_64")]
const VM_GUEST_ELF_MACHINE: u16 = 62;
#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
compile_error!("Evaluator VM guests are only supported on aarch64 and x86_64 hosts");

/// VM substrate installed on one evaluation-capable host.
pub(crate) struct PreparedVmHost {
    vmm: PathBuf,
    runtime_image: PathBuf,
    cache: PathBuf,
}

impl PreparedVmHost {
    /// Opens the fixed host installation or fails without repairing it.
    pub(crate) fn open() -> Result<Self> {
        let started_at = Instant::now();
        let vmm = fs::canonicalize(std::env::current_exe()?)
            .wrap_err("failed to resolve the running Nanocodex executable")?;
        validate_hypervisor_entitlement(&vmm)?;

        let runtime = installed_guest_runtime(&vmm)?;
        let bytes = fs::read(&runtime)
            .wrap_err_with(|| format!("failed to read VM guest runtime {}", runtime.display()))?;
        validate_vm_guest_elf(&bytes, &runtime)?;

        let cache = default_vm_cache()?;
        fs::create_dir_all(&cache)
            .wrap_err_with(|| format!("failed to create VM cache {}", cache.display()))?;
        let runtime_disk = GuestRuntimeDisk::prepare(&runtime, &cache)?;
        let cache_status = match runtime_disk.status() {
            GuestRuntimeDiskStatus::Hit => "hit",
            GuestRuntimeDiskStatus::Created => "created",
        };
        info!(
            target: "nanocodex_vm",
            duration_ns = u64::try_from(started_at.elapsed().as_nanos()).unwrap_or(u64::MAX),
            vm_guest_target = VM_GUEST_TARGET,
            vm_guest_runtime_source = "prepared_host",
            vm_guest_runtime_cache_status = cache_status,
            vm_guest_runtime_digest = runtime_disk.digest(),
            vm_guest_runtime_disk = %runtime_disk.path().display(),
            "VM guest runtime ready"
        );
        Ok(Self {
            vmm,
            runtime_image: runtime_disk.path().to_path_buf(),
            cache,
        })
    }

    pub(crate) fn vmm(&self) -> &Path {
        &self.vmm
    }

    pub(crate) fn runtime_image(&self) -> &Path {
        &self.runtime_image
    }

    pub(crate) fn cache(&self) -> &Path {
        &self.cache
    }
}

fn installed_guest_runtime(vmm: &Path) -> Result<PathBuf> {
    let directory = vmm.parent().ok_or_else(|| {
        eyre!(
            "Nanocodex executable has no installation directory: {}",
            vmm.display()
        )
    })?;
    let runtime = directory.join("nanocodex-vm-guest");
    if !runtime.is_file() {
        return Err(eyre!(
            "prepared eval host is missing {}; install the matching {VM_GUEST_TARGET} \
             `nanocodex-vm-guest` beside the Nanocodex executable (source checkouts can run \
             `just build-eval-host`)",
            runtime.display()
        ));
    }
    fs::canonicalize(&runtime)
        .wrap_err_with(|| format!("failed to resolve VM guest runtime {}", runtime.display()))
}

fn default_vm_cache() -> Result<PathBuf> {
    if let Some(home) = std::env::var_os("NANOCODEX_HOME").filter(|path| !path.is_empty()) {
        return Ok(PathBuf::from(home).join("cache/vm"));
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .ok_or_else(|| eyre!("home directory is unavailable; set NANOCODEX_HOME"))?;
    Ok(cache_beneath_home(Path::new(&home)))
}

fn cache_beneath_home(home: &Path) -> PathBuf {
    home.join(".cache/nanocodex/vm")
}

fn validate_vm_guest_elf(bytes: &[u8], path: &Path) -> Result<()> {
    let header = bytes.get(..20).ok_or_else(|| {
        eyre!(
            "VM guest runtime is too short to contain an ELF header: {}",
            path.display()
        )
    })?;
    if &header[..4] != b"\x7fELF" {
        return Err(eyre!(
            "VM guest runtime is not an ELF executable: {}",
            path.display()
        ));
    }
    let class = header[4];
    let byte_order = header[5];
    let machine = u16::from_le_bytes([header[18], header[19]]);
    if class != 2 || byte_order != 1 || machine != VM_GUEST_ELF_MACHINE {
        return Err(eyre!(
            "VM guest runtime {} has ELF class {class}, byte order {byte_order}, and e_machine \
             {machine}; this host requires the matching {VM_GUEST_TARGET} runtime (64-bit \
             little-endian e_machine {VM_GUEST_ELF_MACHINE})",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn validate_hypervisor_entitlement(vmm: &Path) -> Result<()> {
    let output = std::process::Command::new("/usr/bin/codesign")
        .args(["-d", "--entitlements", "-"])
        .arg(vmm)
        .output()
        .wrap_err("failed to inspect the Nanocodex code signature")?;
    let mut report = output.stdout;
    report.extend_from_slice(&output.stderr);
    if !output.status.success() || !has_hypervisor_entitlement(&report) {
        return Err(eyre!(
            "prepared eval host lacks the macOS `com.apple.security.hypervisor` entitlement: \
             {} (source checkouts can run `just build-eval-host`)",
            vmm.display()
        ));
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
const fn validate_hypervisor_entitlement(_vmm: &Path) -> Result<()> {
    Ok(())
}

#[cfg(any(target_os = "macos", test))]
fn has_hypervisor_entitlement(report: &[u8]) -> bool {
    String::from_utf8_lossy(report).contains("com.apple.security.hypervisor")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installed_runtime_is_a_sibling_of_the_vmm() {
        let directory = tempfile::tempdir().unwrap();
        let vmm = directory.path().join("nanocodex");
        let runtime = directory.path().join("nanocodex-vm-guest");
        fs::write(&runtime, b"runtime").unwrap();

        assert_eq!(
            installed_guest_runtime(&vmm).unwrap(),
            fs::canonicalize(runtime).unwrap()
        );
    }

    #[test]
    fn missing_installed_runtime_fails_with_the_preparation_contract() {
        let directory = tempfile::tempdir().unwrap();
        let error = installed_guest_runtime(&directory.path().join("nanocodex")).unwrap_err();

        assert!(error.to_string().contains("prepared eval host is missing"));
        assert!(error.to_string().contains("just build-eval-host"));
    }

    #[test]
    fn host_cache_is_absolute_and_not_checkout_relative() {
        assert_eq!(
            cache_beneath_home(Path::new("/home/evaluator")),
            Path::new("/home/evaluator/.cache/nanocodex/vm")
        );
    }

    #[test]
    fn entitlement_report_requires_the_hypervisor_key() {
        assert!(has_hypervisor_entitlement(
            b"[Key] com.apple.security.hypervisor\n[Bool] true"
        ));
        assert!(!has_hypervisor_entitlement(b"[Dict]\n"));
    }
}
