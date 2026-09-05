#![allow(
    unsafe_code,
    reason = "this module is one of the two audited libkrun FFI boundaries"
)]

use std::collections::BTreeSet;

use crate::krun::VmError;

/// Optional functionality compiled into the pinned libkrun build.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(u64)]
pub enum KrunFeature {
    /// Virtio networking support.
    Network = 0,
    /// Virtio block-device support.
    Block = 1,
    /// GPU device support.
    Gpu = 2,
    /// Guest input-device support.
    Input = 4,
    /// Trusted-execution-environment support.
    Tee = 6,
    /// AMD SEV confidential-computing support.
    AmdSev = 7,
    /// Intel TDX confidential-computing support.
    IntelTdx = 8,
    /// AWS Nitro enclave support.
    AwsNitro = 9,
    /// `VirGL` resource-map version 2 support.
    VirglResourceMap2 = 10,
    /// Embedded guest init-blob support.
    InitBlob = 11,
}

const FEATURES: [KrunFeature; 10] = [
    KrunFeature::Network,
    KrunFeature::Block,
    KrunFeature::Gpu,
    KrunFeature::Input,
    KrunFeature::Tee,
    KrunFeature::AmdSev,
    KrunFeature::IntelTdx,
    KrunFeature::AwsNitro,
    KrunFeature::VirglResourceMap2,
    KrunFeature::InitBlob,
];

/// Host and build capabilities relevant when configuring a VM.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Capabilities {
    max_vcpus: u32,
    nested_virtualization: bool,
    pause_resume: bool,
    features: BTreeSet<KrunFeature>,
}

impl Capabilities {
    /// Queries the active libkrun build and host hypervisor.
    ///
    /// # Errors
    ///
    /// Returns an error when libkrun cannot query a capability.
    pub fn detect() -> Result<Self, VmError> {
        let max_vcpus = positive(krun::krun_get_max_vcpus(), "query maximum vCPUs")?;
        // SAFETY: this libkrun function has no pointer arguments or additional
        // preconditions despite being declared unsafe in its Rust ABI.
        let nested_virtualization = bool_status(
            unsafe { krun::krun_check_nested_virt() },
            "query nested virtualization",
        )?;
        let mut features = BTreeSet::new();
        for feature in FEATURES {
            if bool_status(
                krun::krun_has_feature(feature as u64),
                "query compiled feature",
            )? {
                features.insert(feature);
            }
        }

        Ok(Self {
            max_vcpus,
            nested_virtualization,
            pause_resume: cfg!(target_os = "macos"),
            features,
        })
    }

    /// Returns the maximum number of virtual CPUs supported by the host.
    #[must_use]
    pub const fn max_vcpus(&self) -> u32 {
        self.max_vcpus
    }

    /// Returns whether the host can expose nested virtualization.
    #[must_use]
    pub const fn nested_virtualization(&self) -> bool {
        self.nested_virtualization
    }

    /// Whether the host supports out-of-band VM pause and resume.
    #[must_use]
    pub const fn pause_resume(&self) -> bool {
        self.pause_resume
    }

    /// Returns whether the pinned libkrun build includes `feature`.
    #[must_use]
    pub fn has(&self, feature: KrunFeature) -> bool {
        self.features.contains(&feature)
    }

    /// Iterates over optional features included in the pinned libkrun build.
    pub fn features(&self) -> impl Iterator<Item = KrunFeature> + '_ {
        self.features.iter().copied()
    }
}

fn positive(status: i32, operation: &'static str) -> Result<u32, VmError> {
    u32::try_from(status).map_err(|_| VmError::Libkrun {
        operation,
        errno: status.saturating_neg(),
    })
}

const fn bool_status(status: i32, operation: &'static str) -> Result<bool, VmError> {
    match status {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(VmError::Libkrun {
            operation,
            errno: status.saturating_neg(),
        }),
    }
}
