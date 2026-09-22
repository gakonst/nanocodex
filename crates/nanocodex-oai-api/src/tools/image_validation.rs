//! Shared envelope validation for embedded outputs and restored history.

/// Checks image MIME and base64 syntax without decoding image pixels.
#[must_use]
pub fn valid_tool_image_data_url(url: &str) -> bool {
    let Some((header, encoded)) = url.split_once(',') else {
        return false;
    };
    let header = header.to_ascii_lowercase();
    let Some(subtype) = header
        .strip_prefix("data:image/")
        .and_then(|v| v.strip_suffix(";base64"))
    else {
        return false;
    };
    if subtype.is_empty()
        || !subtype
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"!#$&^_.+%-".contains(&c))
    {
        return false;
    }
    let bytes = encoded.as_bytes();
    if bytes.is_empty() || bytes.len() % 4 != 0 {
        return false;
    }
    let padding = if encoded.ends_with("==") {
        2
    } else if encoded.ends_with('=') {
        1
    } else {
        0
    };
    let payload = &bytes[..bytes.len() - padding];
    if !payload
        .iter()
        .all(|c| c.is_ascii_alphanumeric() || *c == b'+' || *c == b'/')
    {
        return false;
    }
    // Canonical base64 requires unused bits in the final sextet to be zero.
    // Strict decoders reject otherwise syntactically valid strings like YR==.
    let last = payload[payload.len() - 1];
    let sextet = match last {
        b'A'..=b'Z' => last - b'A',
        b'a'..=b'z' => last - b'a' + 26,
        b'0'..=b'9' => last - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => return false,
    };
    match padding {
        2 => sextet & 15 == 0,
        1 => sextet & 3 == 0,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        ImageDetail,
        tools::{ToolOutputBody, ToolOutputContent},
    };

    #[test]
    fn rejects_noncanonical_padding_and_truncated_payloads() {
        for encoded in [
            "AB==", "AAB=", "YR==", "YWJ=", "====", "A===", "AA=A", "YQ", "", "AAAA\n",
        ] {
            assert!(
                !super::valid_tool_image_data_url(&format!("data:image/png;base64,{encoded}")),
                "{encoded:?}"
            );
        }
        for encoded in ["YQ==", "YWI=", "YWJj", "/w==", "//8="] {
            assert!(
                super::valid_tool_image_data_url(&format!("data:image/png;base64,{encoded}")),
                "{encoded:?}"
            );
        }
    }

    #[test]
    fn raw_tool_output_cannot_bypass_image_envelope_validation() {
        let good = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC";
        let mut output = ToolOutputBody::Content(vec![
            ToolOutputContent::InputText {
                text: "retained".into(),
            },
            ToolOutputContent::InputImage {
                image_url: good.into(),
                detail: ImageDetail::Original,
            },
            ToolOutputContent::InputImage {
                image_url: "data:image/png;base64,AAAA\n[output truncated]".into(),
                detail: ImageDetail::Auto,
            },
            ToolOutputContent::InputImage {
                image_url: "data:text/plain;base64,YQ==".into(),
                detail: ImageDetail::Auto,
            },
        ]);
        output.replace_invalid_image_envelopes();
        let ToolOutputBody::Content(items) = output else {
            panic!("expected content");
        };
        assert!(matches!(&items[0], ToolOutputContent::InputText { text } if text == "retained"));
        assert!(
            matches!(&items[1], ToolOutputContent::InputImage { image_url, detail: ImageDetail::Original } if image_url == good)
        );
        assert!(
            items[2..]
                .iter()
                .all(|item| matches!(item, ToolOutputContent::InputText { .. }))
        );
    }
}
