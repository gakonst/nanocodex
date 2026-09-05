//! Process limits required only by the embedded Tempo egress proxy.

use eyre::{Result, WrapErr};

#[cfg(unix)]
const MPP_FILE_DESCRIPTOR_TARGET: nix::libc::rlim_t = 4_096;

#[cfg(unix)]
pub(crate) fn ensure_mpp_file_descriptor_capacity() -> Result<()> {
    use nix::sys::resource::{RLIM_INFINITY, Resource, getrlimit, setrlimit};

    let (soft_limit, hard_limit) = getrlimit(Resource::RLIMIT_NOFILE)
        .wrap_err("failed to read the process file-descriptor limit")?;
    let target = target_soft_limit(soft_limit, hard_limit, RLIM_INFINITY);
    if target > soft_limit {
        setrlimit(Resource::RLIMIT_NOFILE, target, hard_limit)
            .wrap_err("failed to raise the process file-descriptor limit for MPP egress")?;
    }
    Ok(())
}

#[cfg(not(unix))]
pub(crate) const fn ensure_mpp_file_descriptor_capacity() -> Result<()> {
    Ok(())
}

#[cfg(unix)]
const fn target_soft_limit(
    soft_limit: nix::libc::rlim_t,
    hard_limit: nix::libc::rlim_t,
    infinity: nix::libc::rlim_t,
) -> nix::libc::rlim_t {
    if soft_limit >= MPP_FILE_DESCRIPTOR_TARGET {
        soft_limit
    } else if hard_limit == infinity || hard_limit >= MPP_FILE_DESCRIPTOR_TARGET {
        MPP_FILE_DESCRIPTOR_TARGET
    } else {
        hard_limit
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn preserves_an_existing_larger_soft_limit() {
        assert_eq!(target_soft_limit(8_192, 16_384, u64::MAX), 8_192);
    }

    #[test]
    fn raises_to_the_bounded_mpp_target() {
        assert_eq!(target_soft_limit(256, 16_384, u64::MAX), 4_096);
        assert_eq!(target_soft_limit(256, u64::MAX, u64::MAX), 4_096);
    }

    #[test]
    fn respects_a_smaller_hard_limit() {
        assert_eq!(target_soft_limit(256, 1_024, u64::MAX), 1_024);
    }
}
