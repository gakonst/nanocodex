//! Public ImageIO encoding and NSScreen metadata. Encoding works on generated
//! CGImages without Screen Recording permission, UI, or clipboard access.
use super::{Displays, Encoding, Screen};
use crate::{Error, Result, native::Image};
use base64::Engine as _;
use core_foundation::{
    base::{CFRelease, TCFType},
    data::CFData,
    dictionary::CFDictionary,
    number::CFNumber,
    string::{CFString, CFStringRef},
};
use objc2::MainThreadMarker;
use objc2_app_kit::NSScreen;
use std::{ffi::c_void, ptr};

pub(crate) fn displays() -> Result<Displays> {
    let main = MainThreadMarker::new().ok_or_else(|| {
        Error::action("Native screenshot display metadata requires the main thread")
    })?;
    let screens = NSScreen::screens(main);
    if screens.len() > 64 {
        return Err(Error::action("Too many screenshot displays"));
    }
    Ok(Displays(
        screens
            .iter()
            .map(|s| {
                let r = s.frame();
                Screen {
                    frame: [r.origin.x, r.origin.y, r.size.width, r.size.height],
                    backing_scale: s.backingScaleFactor(),
                }
            })
            .collect(),
    ))
}

#[link(name = "ImageIO", kind = "framework")]
unsafe extern "C" {
    fn CGImageDestinationCreateWithData(
        data: *mut c_void,
        kind: *const c_void,
        count: usize,
        options: *const c_void,
    ) -> *mut c_void;
    fn CGImageDestinationAddImage(
        destination: *mut c_void,
        image: *mut c_void,
        properties: *const c_void,
    );
    fn CGImageDestinationFinalize(destination: *mut c_void) -> bool;
    static kCGImageDestinationLossyCompressionQuality: CFStringRef;
}
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGImageGetWidth(image: *mut c_void) -> usize;
    fn CGImageGetHeight(image: *mut c_void) -> usize;
}
struct Owned(*mut c_void);
impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) };
        }
    }
}

/// # Safety
/// `image` must be a valid, retained CGImage throughout this synchronous call.
/// A callback may borrow it for this invocation; no image pointer escapes.
pub(crate) unsafe fn encode(
    image: *mut c_void,
    encoding: Encoding,
    expected: [usize; 2],
) -> Result<Image> {
    unsafe {
        if image.is_null()
            || expected.into_iter().any(|n| n == 0 || n > 2048)
            || expected[0]
                .checked_mul(expected[1])
                .is_none_or(|n| n > 2048 * 768)
            || [CGImageGetWidth(image), CGImageGetHeight(image)] != expected
        {
            return Err(Error::action(
                "Captured image dimensions do not match screenshot geometry",
            ));
        }
        let data = core_foundation::data::CFDataCreateMutable(ptr::null(), 0);
        if data.is_null() {
            return Err(Error::action("Cannot allocate screenshot data"));
        }
        let _data_owner = Owned(data.cast());
        let kind = CFString::new(match encoding {
            Encoding::Jpeg { .. } => "public.jpeg",
            Encoding::Png => "public.png",
        });
        let dest =
            CGImageDestinationCreateWithData(data.cast(), kind.as_CFTypeRef(), 1, ptr::null());
        if dest.is_null() {
            return Err(Error::action("Cannot allocate image encoder"));
        }
        let dest = Owned(dest);
        let properties = match encoding {
            Encoding::Jpeg { quality: Some(q) } => {
                // The trusted config removes non-finite/out-of-range quality.
                // Direct Rust construction still fails rather than forwarding NaN.
                if !q.is_finite() {
                    return Err(Error::invalid("JPEG quality must be finite"));
                }
                let key = CFString::wrap_under_get_rule(kCGImageDestinationLossyCompressionQuality);
                Some(CFDictionary::from_CFType_pairs(&[(
                    key.as_CFType(),
                    CFNumber::from(q.clamp(0.0, 1.0)).as_CFType(),
                )]))
            }
            _ => None,
        };
        CGImageDestinationAddImage(
            dest.0,
            image,
            properties
                .as_ref()
                .map_or(ptr::null(), |p| p.as_CFTypeRef()),
        );
        if !CGImageDestinationFinalize(dest.0) {
            return Err(Error::action("Screenshot image finalization failed"));
        }
        // Keep the original data owner alive through destination teardown.
        let bytes = CFData::wrap_under_get_rule(data.cast());
        if bytes.bytes().len() > 32 * 1024 * 1024 {
            return Err(Error::action("Encoded screenshot exceeds 32 MiB"));
        }
        Ok(Image {
            mime_type: encoding.mime_type().into(),
            data: base64::engine::general_purpose::STANDARD.encode(bytes.bytes()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGColorSpaceCreateDeviceRGB() -> *mut c_void;
        fn CGDataProviderCreateWithCFData(data: *const c_void) -> *mut c_void;
        fn CGImageCreate(
            width: usize,
            height: usize,
            component_bits: usize,
            pixel_bits: usize,
            stride: usize,
            color: *mut c_void,
            bitmap: u32,
            provider: *mut c_void,
            decode: *const c_void,
            interpolate: bool,
            intent: u32,
        ) -> *mut c_void;
    }
    fn generated_image(width: usize, height: usize) -> Owned {
        let rgba: Vec<u8> = (0..width * height)
            .flat_map(|i| {
                [
                    ((i % width) * 255 / width) as u8,
                    ((i / width) * 255 / height) as u8,
                    ((i * 31) % 256) as u8,
                    255,
                ]
            })
            .collect();
        let data = CFData::from_buffer(&rgba);
        unsafe {
            let color = Owned(CGColorSpaceCreateDeviceRGB());
            let provider = Owned(CGDataProviderCreateWithCFData(data.as_CFTypeRef()));
            assert!(!color.0.is_null() && !provider.0.is_null());
            let result = Owned(CGImageCreate(
                width,
                height,
                8,
                32,
                width * 4,
                color.0,
                3,
                provider.0,
                ptr::null(),
                false,
                0,
            ));
            assert!(!result.0.is_null());
            result
        }
    }
    fn decoded(encoding: Encoding) -> (Vec<u8>, image::DynamicImage) {
        let generated = generated_image(128, 96);
        let encoded = unsafe { encode(generated.0, encoding, [128, 96]) }.unwrap();
        assert_eq!(encoded.mime_type, encoding.mime_type());
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded.data)
            .unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (128, 96));
        (bytes, decoded)
    }
    #[test]
    fn generated_cgimage_default_jpeg_has_actual_jpeg_bytes_and_size() {
        let (bytes, decoded) = decoded(super::super::Configuration::default().encoding);
        assert_eq!(&bytes[..2], &[0xff, 0xd8]);
        assert_eq!(&bytes[bytes.len() - 2..], &[0xff, 0xd9]);
        let rgb = decoded.to_rgb8();
        // Verify meaningful gradient content, without claiming lossy exact pixels.
        assert!(rgb.get_pixel(110, 80)[0] > rgb.get_pixel(10, 80)[0] + 100);
        assert!(rgb.get_pixel(110, 80)[1] > rgb.get_pixel(110, 10)[1] + 100);
    }
    #[test]
    fn generated_cgimage_png_preserves_pixels_and_jpeg_quality_changes_encoding() {
        let (png, decoded_image) = decoded(Encoding::Png);
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        let rgba = decoded_image.to_rgba8();
        let i = 30 * 128 + 40;
        assert_eq!(
            rgba.get_pixel(40, 30).0,
            [
                (40usize * 255 / 128) as u8,
                (30usize * 255 / 96) as u8,
                ((i * 31) % 256) as u8,
                255
            ]
        );
        let low = decoded(Encoding::Jpeg { quality: Some(0.1) }).0;
        let high = decoded(Encoding::Jpeg { quality: Some(1.0) }).0;
        assert!(high.len() > low.len());
        assert_ne!(high, low);
        decoded(Encoding::Jpeg { quality: None });
    }
    #[test]
    fn encoder_rejects_geometry_mismatch_and_nonfinite_quality_before_output() {
        let generated = generated_image(16, 8);
        assert!(unsafe { encode(generated.0, Encoding::Png, [15, 8]) }.is_err());
        assert!(
            unsafe {
                encode(
                    generated.0,
                    Encoding::Jpeg {
                        quality: Some(f64::NAN),
                    },
                    [16, 8],
                )
            }
            .is_err()
        );
        assert!(unsafe { encode(ptr::null_mut(), Encoding::Png, [16, 8]) }.is_err());
    }
}
