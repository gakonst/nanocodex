//! Session identity reaches every process a Claude tool launches: real Bash
//! subprocesses through the public adapter, and the PDF helpers behind Read.
#![cfg(unix)]

use nanocodex_claude_tools::{
    BashRequest, BashResult, ClaudeBash, ClaudeWorkspaceFiles, MediaReadOptions,
    SandboxBashExecutor, SessionEnvironment,
};
use serde_json::{Value, json};
use std::{fs, io::Cursor, os::unix::fs::PermissionsExt as _, process::Command};

/// A host executor that runs the command in a real bash subprocess. The host
/// environment it configures includes spoofed identities, which the bound
/// session identity must override.
struct RealBash;

impl SandboxBashExecutor for RealBash {
    async fn execute(&self, request: BashRequest) -> Result<BashResult, String> {
        let mut command = Command::new("bash");
        command
            .arg("-c")
            .arg(&request.command)
            .env(SessionEnvironment::SESSION_ID_VAR, "spoofed-session")
            .env(SessionEnvironment::ROOT_SESSION_ID_VAR, "spoofed-root");
        request.apply_session(&mut command);
        let output = command.output().map_err(|error| error.to_string())?;
        Ok(BashResult {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            exit_code: output.status.code().unwrap_or(-1),
            truncated: false,
        })
    }
}

const PRINT_IDS: &str = r#"printf '%s|%s|%s' "${CODEX_THREAD_ID-unset}" "${NANOCODEX_ROOT_SESSION_ID-unset}" "$(env | grep -c spoofed)""#;

async fn observed(bash: &ClaudeBash<RealBash>, session: Option<SessionEnvironment>) -> String {
    let output = bash
        .execute_in_session("Bash", json!({ "command": PRINT_IDS }), session)
        .await
        .unwrap();
    let output: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(output["exit_code"], 0, "{output}");
    output["stdout"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn bash_subprocesses_export_their_launching_session() {
    let bash = ClaudeBash::new(RealBash);

    // A root exports its own id for both variables, overriding the spoof.
    let root = SessionEnvironment::root("root-session");
    assert_eq!(
        observed(&bash, Some(root.clone())).await,
        "root-session|root-session|0"
    );

    // A forked or child session exports its own id and the shared root.
    let child = SessionEnvironment::new("child-session", root.session_id());
    assert_eq!(
        observed(&bash, Some(child)).await,
        "child-session|root-session|0"
    );

    // Without a session, no identity (spoofed or inherited) reaches the command.
    assert_eq!(observed(&bash, None).await, "unset|unset|0");
}

#[tokio::test]
async fn pdf_helpers_export_their_launching_session() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("page.pdf"), b"%PDF-1.4\n%%EOF\n").unwrap();
    let image =
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(4, 3, image::Rgb([20, 40, 80])));
    let mut png = Cursor::new(Vec::new());
    image.write_to(&mut png, image::ImageFormat::Png).unwrap();
    let raster = dir.path().join("raster.png");
    fs::write(&raster, png.into_inner()).unwrap();
    let helpers = tempfile::tempdir().unwrap();
    let marker = helpers.path().join("ids");
    let script = |name: &str, body: String| {
        let path = helpers.path().join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    };
    let record = format!(
        r#"printf '%s %s|%s\n' "$(basename "$0")" "${{CODEX_THREAD_ID-unset}}" "${{NANOCODEX_ROOT_SESSION_ID-unset}}" >> '{}'"#,
        marker.display()
    );
    let options = MediaReadOptions {
        pdfinfo: script("pdfinfo", format!("{record}\necho 'Pages:          1'")),
        pdftoppm: script("pdftoppm", format!("{record}\ncat '{}'", raster.display())),
    };
    let files = ClaudeWorkspaceFiles::new(dir.path())
        .unwrap()
        .with_media_options(options);

    files
        .execute_output_in_session(
            "Read",
            json!({"file_path":"page.pdf"}),
            false,
            Some(SessionEnvironment::new("child-session", "root-session")),
        )
        .await
        .unwrap();
    assert_eq!(
        fs::read_to_string(&marker).unwrap(),
        "pdfinfo child-session|root-session\npdftoppm child-session|root-session\n"
    );

    fs::remove_file(&marker).unwrap();
    files
        .execute_output("Read", json!({"file_path":"page.pdf"}))
        .await
        .unwrap();
    assert_eq!(
        fs::read_to_string(&marker).unwrap(),
        "pdfinfo unset|unset\npdftoppm unset|unset\n"
    );
}
