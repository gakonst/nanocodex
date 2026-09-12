use crate::{Error, Result};
use base64::Engine as _;
use serde_json::{Value, json};
use std::io::Read;
pub(crate) fn helper_response(result: Result<Value>) -> String {
    match result {
        Ok(value) => json!({"result":value}),
        Err(error) => json!({"error":error}),
    }
    .to_string()
}
pub(crate) fn parse_url(request: &str) -> Result<Value> {
    if request.len() > 128 * 1024 {
        return Err(Error::invalid("URL input exceeds 128 KiB"));
    }
    let args: Value = serde_json::from_str(request)?;
    let input = args["input"]
        .as_str()
        .ok_or_else(|| Error::invalid("Invalid URL"))?;
    let mut url = if let Some(base) = args["base"].as_str() {
        url::Url::parse(base).and_then(|base| base.join(input))
    } else {
        url::Url::parse(input)
    }
    .map_err(|_| Error::invalid("Invalid URL"))?;
    if let (Some(property), Some(value)) = (
        args["set"]["property"].as_str(),
        args["set"]["value"].as_str(),
    ) {
        match property {
            "href" => url = url::Url::parse(value).map_err(|_| Error::invalid("Invalid URL"))?,
            "protocol" => {
                let _ = url.set_scheme(value.trim_end_matches(':'));
            }
            "username" => {
                let _ = url.set_username(value);
            }
            "password" => {
                let _ = url.set_password(Some(value));
            }
            "hostname" => {
                let _ = url.set_host(Some(value));
            }
            "host" => {
                if let Ok(host) = url::Url::parse(&format!("{}://{value}", url.scheme())) {
                    let _ = url.set_host(host.host_str());
                    let _ = url.set_port(host.port());
                }
            }
            "port" => {
                if value.is_empty() {
                    let _ = url.set_port(None);
                } else if let Ok(port) = value.parse::<u16>() {
                    let _ = url.set_port(Some(port));
                }
            }
            "pathname" => url.set_path(value),
            "search" => url.set_query(if value.is_empty() {
                None
            } else {
                Some(value.strip_prefix('?').unwrap_or(value))
            }),
            "hash" => url.set_fragment(if value.is_empty() {
                None
            } else {
                Some(value.strip_prefix('#').unwrap_or(value))
            }),
            _ => return Err(Error::invalid("Unsupported URL property")),
        }
    }
    let hostname = url.host_str().unwrap_or("");
    let host = match url.port() {
        Some(port) => format!("{hostname}:{port}"),
        None => hostname.into(),
    };
    Ok(
        json!({"href":url.as_str(),"origin":url.origin().ascii_serialization(),"protocol":format!("{}:",url.scheme()),"username":url.username(),"password":url.password().unwrap_or(""),"host":host,"hostname":hostname,"port":url.port().map(|n|n.to_string()).unwrap_or_default(),"pathname":url.path(),"search":url.query().filter(|s|!s.is_empty()).map(|s|format!("?{s}")).unwrap_or_default(),"hash":url.fragment().filter(|s|!s.is_empty()).map(|s|format!("#{s}")).unwrap_or_default()}),
    )
}
const MAX_IMAGE_BYTES: usize = 3 * 1024 * 1024;
fn image_mime(bytes: &[u8]) -> Result<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Ok("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Ok("image/jpeg")
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP".as_slice()) {
        Ok("image/webp")
    } else {
        Err(Error::invalid(
            "nodeRepl.emitImage could not infer image MIME type from bytes; expected PNG, JPEG, or WebP data",
        ))
    }
}
pub(crate) fn read_image_file(value: &str) -> Result<Value> {
    if value.len() > 128 * 1024 {
        return Err(Error::invalid("Image file URL exceeds 128 KiB"));
    }
    let url = url::Url::parse(value).map_err(|_| Error::invalid("Invalid image file URL"))?;
    let path = url
        .to_file_path()
        .map_err(|_| Error::invalid("Expected a local file URL"))?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(Error::invalid("Image file must be a regular file"));
    }
    if metadata.len() > MAX_IMAGE_BYTES as u64 {
        return Err(Error::invalid(
            "Image exceeds the bounded 3 MiB image budget",
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_IMAGE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(Error::invalid(
            "Image exceeds the bounded 3 MiB image budget",
        ));
    }
    let mime = image_mime(&bytes)?;
    Ok(json!({"data":base64::engine::general_purpose::STANDARD.encode(bytes),"mime_type":mime}))
}
pub(crate) fn image_data_url(value: &str) -> Result<Value> {
    if value.len() > 4 * 1024 * 1024 {
        return Err(Error::invalid("Image exceeds the bounded data URL budget"));
    }
    let (metadata, data) = value
        .get(5..)
        .and_then(|s| s.split_once(','))
        .ok_or_else(|| Error::invalid("Invalid image data URL"))?;
    let mime = metadata
        .split(';')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("text/plain");
    let bytes = if metadata
        .split(';')
        .any(|part| part.eq_ignore_ascii_case("base64"))
    {
        let data = data
            .bytes()
            .filter(|b| !b.is_ascii_whitespace())
            .collect::<Vec<_>>();
        base64::engine::general_purpose::STANDARD
            .decode(&data)
            .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(&data))
            .map_err(|_| Error::invalid("Invalid image base64 data"))?
    } else {
        let mut bytes = Vec::new();
        let mut input = data.bytes();
        while let Some(byte) = input.next() {
            if byte == b'%' {
                let hi = input.next().and_then(|b| (b as char).to_digit(16));
                let lo = input.next().and_then(|b| (b as char).to_digit(16));
                match (hi, lo) {
                    (Some(hi), Some(lo)) => bytes.push((hi * 16 + lo) as u8),
                    _ => return Err(Error::invalid("Invalid percent encoding in image data URL")),
                }
            } else {
                bytes.push(byte);
            }
        }
        bytes
    };
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(Error::invalid(
            "Image exceeds the bounded 3 MiB image budget",
        ));
    }
    Ok(json!({"data":base64::engine::general_purpose::STANDARD.encode(bytes),"mime_type":mime}))
}
