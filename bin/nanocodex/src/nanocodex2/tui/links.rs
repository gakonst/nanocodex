//! Resolve logical workspace paths through the owning agent before opening them locally.
use nanocodex_managed::{ManagedClient, ManagedError};
use std::path::Path;

pub(super) async fn open(
    client: &ManagedClient,
    agent_id: &str,
    destination: &str,
) -> Result<(), String> {
    open_with(client, agent_id, destination, |path| async move {
        launch(&path).await
    })
    .await
}

async fn open_with<F, Fut>(
    client: &ManagedClient,
    agent_id: &str,
    destination: &str,
    opener: F,
) -> Result<(), String>
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    let Some(path) = file_path(destination)? else {
        return opener(destination.to_owned()).await;
    };
    if !path.starts_with('/') {
        return opener(path).await;
    }
    let filename = Path::new(&path)
        .file_name()
        .ok_or_else(|| "This link does not name a file.".to_owned())?;
    let directory = tempfile::Builder::new()
        .prefix("nanocodex-open-")
        .tempdir()
        .map_err(|error| format!("Could not prepare file download: {error}"))?;
    let local = directory.path().join(filename);
    match client.download_file(agent_id, &path, &local).await {
        Ok(_) => {
            // The viewer may read lazily or outlive this process. Retain the private
            // temporary directory only after a complete download and successful open.
            opener(
                local
                    .to_str()
                    .ok_or("Invalid downloaded file path")?
                    .to_owned(),
            )
            .await?;
            let _ = directory.keep();
            Ok(())
        }
        Err(ManagedError::Http { code, .. }) if code == "file_path_unmapped" => opener(path).await,
        Err(error) => Err(format!("Could not open {path}: {error}")),
    }
}

fn file_path(destination: &str) -> Result<Option<String>, String> {
    if destination.starts_with("file:") {
        let url = url::Url::parse(destination).map_err(|_| "Invalid file link")?;
        if url.host_str().is_some_and(|host| host != "localhost") {
            return Err("File links with a remote hostname are unsupported.".to_owned());
        }
        // Logical Hand paths use a POSIX namespace even on Windows, where
        // Url::to_file_path rejects paths without a drive letter.
        #[cfg(windows)]
        if url.path().as_bytes().get(2) == Some(&b':') {
            let path = url.to_file_path().map_err(|_| "Invalid file link")?;
            return Ok(Some(strip_location(&path.to_string_lossy()).to_owned()));
        }
        let path = percent_encoding::percent_decode_str(url.path())
            .decode_utf8()
            .map_err(|_| "Invalid UTF-8 in file link")?;
        if path.contains('\0') {
            return Err("Invalid file link".to_owned());
        }
        return Ok(Some(strip_location(&path).to_owned()));
    }
    // Absolute paths can contain colons, hashes and question marks. They are
    // filesystem names, not URLs; only decode Markdown's URL escaping.
    if destination.starts_with('/') || url::Url::parse(destination).is_err() {
        let path = percent_encoding::percent_decode_str(destination)
            .decode_utf8()
            .map_err(|_| "Invalid UTF-8 in file link")?;
        if path.contains('\0') {
            return Err("Invalid file link".to_owned());
        }
        return Ok(Some(strip_location(&path).to_owned()));
    }
    Ok(None)
}

fn strip_location(path: &str) -> &str {
    // The model's documented file-link format allows :line and :line:column.
    let Some((base, suffix)) = path.rsplit_once(':') else {
        return path;
    };
    if suffix.is_empty() || !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
        return path;
    }
    if let Some((file, line)) = base.rsplit_once(':')
        && !line.is_empty()
        && line.bytes().all(|byte| byte.is_ascii_digit())
    {
        return file;
    }
    base
}

async fn launch(destination: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let mut command = tokio::process::Command::new("open");
    #[cfg(target_os = "linux")]
    let mut command = tokio::process::Command::new("xdg-open");
    #[cfg(target_os = "windows")]
    let mut command = tokio::process::Command::new("explorer.exe");
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    return Err("Opening links is unsupported on this platform.".to_owned());
    let status = command
        .arg(destination)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .map_err(|error| format!("Could not open link: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("Could not open link ({status})"))
    }
}

#[cfg(test)]
mod tests {
    use super::file_path;

    #[test]
    fn remote_deliverables_keep_their_hand_and_filename() {
        for path in [
            "/brain/outputs/gak-stack-r4/preview.jpg",
            "/omarchy-desktop/gpu-stack-delivery/GAK-STACK-R4-complete.zip",
            "/other-hand/folder/a#b?c.zip",
        ] {
            assert_eq!(file_path(path).unwrap().as_deref(), Some(path));
        }
        assert_eq!(
            file_path("/other-hand/My%20Report.pdf").unwrap().as_deref(),
            Some("/other-hand/My Report.pdf")
        );
    }

    #[test]
    fn source_locations_are_not_part_of_the_download_name() {
        assert_eq!(
            file_path("/hand/src/main.rs:12").unwrap().as_deref(),
            Some("/hand/src/main.rs")
        );
        assert_eq!(
            file_path("/hand/src/main.rs:12:4").unwrap().as_deref(),
            Some("/hand/src/main.rs")
        );
        assert_eq!(
            file_path("/hand/report:final.pdf").unwrap().as_deref(),
            Some("/hand/report:final.pdf")
        );
    }

    #[test]
    fn web_links_stay_external_and_file_urls_are_decoded() {
        assert_eq!(file_path("https://example.com/a.pdf?q=1").unwrap(), None);
        assert_eq!(
            file_path("file:///brain/My%20Report.pdf")
                .unwrap()
                .as_deref(),
            Some("/brain/My Report.pdf")
        );
        assert_eq!(
            file_path("file://localhost/other-hand/My%20Report.pdf:12")
                .unwrap()
                .as_deref(),
            Some("/other-hand/My Report.pdf")
        );
        assert!(file_path("file:///brain/a%00b").is_err());
        assert!(file_path("file:///brain/%FF").is_err());
        assert!(file_path("file://another-box/private/file").is_err());
        assert!(file_path("/brain/a%00b").is_err());
    }

    #[cfg(windows)]
    #[test]
    fn native_windows_file_urls_keep_their_drive() {
        assert_eq!(
            file_path("file:///C:/Users/me/My%20Report.pdf")
                .unwrap()
                .as_deref(),
            Some(r"C:\Users\me\My Report.pdf")
        );
    }

    #[tokio::test]
    async fn remote_file_opens_a_complete_local_copy_and_errors_do_not_fall_back() {
        use axum::{
            Router, extract::Query, http::StatusCode, response::IntoResponse, routing::get,
        };
        use nanocodex_managed::{ManagedApiKey, ManagedClient};
        let app = Router::new().route(
            "/v1/agents/agent-1/files",
            get(
                |Query(query): Query<std::collections::HashMap<String, String>>| async move {
                    match query["path"].as_str() {
                        "/omarchy-desktop/delivery/a.zip" => {
                            vec![0_u8, 255, 80, 75].into_response()
                        }
                        "/Users/me/source.rs" => (
                            StatusCode::NOT_FOUND,
                            axum::Json(serde_json::json!({
                                "error": "file_path_unmapped", "message": "local path",
                            })),
                        )
                            .into_response(),
                        _ => (
                            StatusCode::SERVICE_UNAVAILABLE,
                            axum::Json(serde_json::json!({
                                "error": "hand_offline", "message": "Hand is offline",
                            })),
                        )
                            .into_response(),
                    }
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let key = format!("ncx_live_{}_{}", "a".repeat(12), "b".repeat(43));
        let client = ManagedClient::new(
            format!("http://{address}"),
            ManagedApiKey::parse(key).unwrap(),
        )
        .unwrap();
        let saved = std::sync::Arc::new(std::sync::Mutex::new(None));
        let observed = saved.clone();
        super::open_with(
            &client,
            "agent-1",
            "/omarchy-desktop/delivery/a.zip",
            move |path| async move {
                assert_ne!(path, "/omarchy-desktop/delivery/a.zip");
                assert_eq!(std::fs::read(&path).unwrap(), [0, 255, 80, 75]);
                assert_eq!(std::path::Path::new(&path).file_name().unwrap(), "a.zip");
                *observed.lock().unwrap() = Some(path);
                Ok(())
            },
        )
        .await
        .unwrap();
        let saved = saved.lock().unwrap().take().unwrap();
        assert!(std::path::Path::new(&saved).exists());
        std::fs::remove_dir_all(std::path::Path::new(&saved).parent().unwrap()).unwrap();
        super::open_with(
            &client,
            "agent-1",
            "/Users/me/source.rs:12",
            |path| async move {
                assert_eq!(path, "/Users/me/source.rs");
                Ok(())
            },
        )
        .await
        .unwrap();
        let error = super::open_with(
            &client,
            "agent-1",
            "/omarchy-desktop/offline.zip",
            |_| async { panic!("a managed path must never fall back to the local filesystem") },
        )
        .await
        .unwrap_err();
        assert!(error.contains("Hand is offline"));
        server.abort();
    }
}
