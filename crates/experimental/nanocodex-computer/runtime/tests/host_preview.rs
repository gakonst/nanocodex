use base64::{Engine as _, engine::general_purpose::STANDARD};
use skyre::{native::Image, preview::Preview};
use std::{
    io::{Cursor, Read, Write},
    net::TcpStream,
    thread,
    time::{Duration, Instant},
};
fn start(duration: u64) -> Preview {
    Preview::start(
        "owner",
        "fixture://owned",
        Duration::from_millis(duration),
        Duration::from_millis(250),
    )
    .unwrap()
}
fn image() -> Image {
    let image = image::RgbaImage::from_pixel(2, 2, image::Rgba([10, 20, 30, 255]));
    let mut bytes = Cursor::new(Vec::new());
    image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
    Image {
        mime_type: "image/png".into(),
        data: STANDARD.encode(bytes.into_inner()),
    }
}
fn endpoint(preview: &Preview) -> (String, String) {
    let status = preview.status("owner").unwrap();
    let url = url::Url::parse(status["url"].as_str().unwrap()).unwrap();
    (
        format!("{}:{}", url.host_str().unwrap(), url.port().unwrap()),
        url.path().into(),
    )
}
fn request(address: &str, target: &str, headers: &str) -> String {
    let mut socket = TcpStream::connect(address).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    write!(
        socket,
        "GET {target} HTTP/1.1\r\nHost: {address}\r\n{headers}\r\n"
    )
    .unwrap();
    let mut response = String::new();
    socket.read_to_string(&mut response).unwrap();
    response
}
#[test]
fn host_preview_capability_url_latest_frame_and_no_input_routes() {
    let mut preview = start(3000);
    let (address, path) = endpoint(&preview);
    assert!(preview.due());
    preview.publish("owner", image()).unwrap();
    assert!(!preview.due());
    assert!(preview.status("other").is_err());
    assert!(preview.publish("other", image()).is_err());
    assert!(preview.close("other").is_err());
    let html = request(&address, &path, "");
    assert!(html.starts_with("HTTP/1.1 200"), "response: {html:?}");
    assert!(html.contains("frame-ancestors 'none'"));
    assert!(html.contains("credentials:'omit'"));
    let frame = request(&address, &format!("{path}/frame"), "");
    let frame: serde_json::Value =
        serde_json::from_str(frame.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(frame["sequence"], 1);
    assert_eq!(frame["frame"]["mime_type"], "image/png");
    preview.publish("owner", image()).unwrap();
    assert_eq!(preview.status("owner").unwrap()["sequence"], 2);
    assert!(request(&address, "/wrong", "").starts_with("HTTP/1.1 404"));
    assert!(request(&address, &format!("{path}/click"), "").starts_with("HTTP/1.1 404"));
    assert!(request(&address, &path, "Origin: https://evil.test\r\n").starts_with("HTTP/1.1 403"));
    assert!(
        request(&address, &path, "Cookie: should-not-authorize=1\r\n").starts_with("HTTP/1.1 403")
    );
    assert!(request(&address, &path, "Host: rebinding.test\r\n").starts_with("HTTP/1.1 403"));
    preview.close("owner").unwrap();
    assert!(TcpStream::connect(address).is_err());
    assert!(preview.publish("owner", image()).is_err());
}
#[test]
fn host_preview_validates_image_and_bounds_lifetime() {
    assert!(
        Preview::start(
            "owner",
            "app",
            Duration::from_secs(61),
            Duration::from_millis(250)
        )
        .is_err()
    );
    let mut preview = start(300);
    assert!(
        preview
            .publish(
                "owner",
                Image {
                    mime_type: "image/png".into(),
                    data: "not base64".into()
                }
            )
            .is_err()
    );
    assert!(
        preview
            .publish(
                "owner",
                Image {
                    mime_type: "image/png".into(),
                    data: STANDARD.encode("not png")
                }
            )
            .is_err()
    );
    assert_eq!(preview.status("owner").unwrap()["sequence"], 0);
    let (address, _) = endpoint(&preview);
    thread::sleep(Duration::from_millis(350));
    assert!(preview.closed());
    assert!(TcpStream::connect(address).is_err());
}
#[test]
fn host_preview_slow_client_cannot_hold_shutdown_open() {
    let mut preview = start(3000);
    let (address, _) = endpoint(&preview);
    let mut socket = TcpStream::connect(address).unwrap();
    let writer = thread::spawn(move || {
        for _ in 0..30 {
            if socket.write_all(b"G").is_err() {
                break;
            }
            thread::sleep(Duration::from_millis(40));
        }
    });
    thread::sleep(Duration::from_millis(80));
    let started = Instant::now();
    preview.close("owner").unwrap();
    assert!(started.elapsed() < Duration::from_millis(750));
    writer.join().unwrap();
}

#[test]
fn host_preview_accepts_fragmented_headers_and_split_terminator() {
    let mut preview = start(3000);
    let (address, path) = endpoint(&preview);
    let mut socket = TcpStream::connect(&address).unwrap();
    socket.set_nodelay(true).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    socket.write_all(b"GET ").unwrap();
    // The listener has time to accept before a complete header exists. This
    // previously failed immediately on macOS's inherited O_NONBLOCK flag.
    thread::sleep(Duration::from_millis(40));
    socket
        .write_all(format!("{path} HTTP/1.1\r\nHost: {address}\r\n\r").as_bytes())
        .unwrap();
    thread::sleep(Duration::from_millis(10));
    socket.write_all(b"\n").unwrap();
    let mut response = String::new();
    socket.read_to_string(&mut response).unwrap();
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "response: {response:?}"
    );
    preview.close("owner").unwrap();
}

#[test]
fn host_preview_buffered_headers_preserve_first_request_and_8192_byte_limit() {
    let mut preview = start(3000);
    let (address, path) = endpoint(&preview);
    let prefix = format!("GET {path} HTTP/1.1\r\nHost: {address}\r\nX-Padding: ");
    let complete = format!("{prefix}{}\r\n\r\n", "x".repeat(8192 - prefix.len() - 4));
    assert_eq!(complete.len(), 8192);
    let incomplete = format!("{prefix}{}", "x".repeat(8192 - prefix.len()));
    let pipelined = format!(
        "GET {path} HTTP/1.1\r\nHost: {address}\r\n\r\nGET /wrong HTTP/1.1\r\nOrigin: https://invalid.test\r\n\r\n"
    );
    for (input, status) in [(complete, "200"), (incomplete, "400"), (pipelined, "200")] {
        let mut socket = TcpStream::connect(&address).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        socket.write_all(input.as_bytes()).unwrap();
        let mut response = String::new();
        let result = socket.read_to_string(&mut response);
        // Closing a single-request connection may reset unread pipelined bytes.
        assert!(
            result.is_ok() || result.unwrap_err().kind() == std::io::ErrorKind::ConnectionReset
        );
        assert!(
            response.starts_with(&format!("HTTP/1.1 {status}")),
            "response: {response:?}"
        );
    }
    preview.close("owner").unwrap();
}

#[test]
fn host_preview_header_deadline_is_absolute_across_fragments() {
    let mut preview = start(3000);
    let (address, path) = endpoint(&preview);
    let mut socket = TcpStream::connect(&address).unwrap();
    socket.set_nodelay(true).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let began = Instant::now();
    let mut write_failed = false;
    for part in ["G", "E", "T", " "] {
        if socket.write_all(part.as_bytes()).is_err() {
            write_failed = true;
            break;
        }
        thread::sleep(Duration::from_millis(90));
    }
    if !write_failed {
        let _ = socket.write_all(format!("{path} HTTP/1.1\r\nHost: {address}\r\n\r\n").as_bytes());
    }
    let mut response = String::new();
    let result = socket.read_to_string(&mut response);
    assert!(result.is_ok() || result.unwrap_err().kind() == std::io::ErrorKind::ConnectionReset);
    assert!(response.is_empty(), "late request was served: {response:?}");
    assert!(began.elapsed() < Duration::from_secs(1));
    preview.close("owner").unwrap();
}
