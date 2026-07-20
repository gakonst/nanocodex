use std::borrow::Cow;
use xxhash_rust::xxh3::xxh3_64;
use xxhash_rust::xxh32::xxh32;

pub(super) const FILE_HASH_WIDTH: usize = 8;
pub(super) const LINE_HASH_WIDTH: usize = 4;

pub(super) fn hash_hex(input: &str) -> String {
    let normalized = normalize_file_text(input);
    hash_normalized_hex(&normalized)
}

pub(super) fn hash_normalized_hex(input: &str) -> String {
    let hash = xxh3_64(input.as_bytes());
    format!("{:08x}", hash >> 32)
}

pub(super) fn line_hash(input: &str) -> String {
    let hash = xxh32(input.as_bytes(), 0) & 0xffff;
    format!("{hash:04x}")
}

pub(super) fn normalize_file_text(input: &str) -> Cow<'_, str> {
    let input = input.strip_prefix('\u{feff}').unwrap_or(input);
    if !input.contains('\r') {
        return Cow::Borrowed(input);
    }

    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            output.push('\n');
        } else {
            output.push(ch);
        }
    }
    Cow::Owned(output)
}
