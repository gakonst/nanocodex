//! Complete official ICU 78.3 GB18030-2022 mapping, compiled by ICU 72.1.
//! The source, license, reproducible commands, and exhaustive original-kernel
//! comparisons are retained under evidence/runtime-icu-data/20260907T000225696697Z.

#[repr(align(16))]
struct Aligned<const N: usize>([u8; N]);

static PACKAGE: Aligned<233008> = Aligned(*include_bytes!("skyre_icu783_gb18030.dat"));

pub const NAME: &std::ffi::CStr = c"skyre_icu783_gb18030";
pub const CONVERTER: &std::ffi::CStr = c"gb18030-2022";
pub const SHA256: &str = "c889173af9119a778669432f36f431a76d3c64818b435e90d8d1d3e7704bf4da";
pub const SOURCE_COMMIT: &str = "21d1eb0f306e1141c10931e914dfc038c06121da";

pub fn bytes() -> &'static [u8] {
    &PACKAGE.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn shipped_converter_package_matches_exhaustively_verified_data() {
        assert_eq!(bytes().as_ptr() as usize % 16, 0);
        assert_eq!(format!("{:x}", Sha256::digest(bytes())), SHA256);
        assert_eq!(&bytes()[12..20], b"CmnD\x01\0\0\0");
    }
}
