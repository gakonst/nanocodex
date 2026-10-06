//! Public adapter journeys: actual image decoding, Poppler rendering, notebooks,
//! and filesystem search. No adapter internals or model mocks are exercised.
use base64::{Engine as _, engine::general_purpose::STANDARD};
use nanocodex_claude_tools::{
    ClaudeNotebook, ClaudeWorkspaceFiles, ImageSource, MediaReadOptions, ToolContent, ToolOutput,
    ToolResultBlock,
};
use serde_json::{Value, json};
use std::{fs, io::Cursor, path::PathBuf};

fn blocks(output: ToolOutput) -> Vec<ToolResultBlock> {
    match output.content {
        ToolContent::Blocks(blocks) => blocks,
        ToolContent::Text(text) => panic!("expected native blocks, got {text}"),
    }
}

fn image_bytes(format: image::ImageFormat) -> Vec<u8> {
    let image =
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(4, 3, image::Rgb([20, 40, 80])));
    let mut out = Cursor::new(Vec::new());
    image.write_to(&mut out, format).unwrap();
    out.into_inner()
}

fn assert_image(block: &ToolResultBlock, expected: &[u8], mime: &str) {
    match block {
        ToolResultBlock::Image {
            source: ImageSource::Base64 { media_type, data },
        } => {
            assert_eq!(media_type, mime);
            assert_eq!(STANDARD.decode(data).unwrap(), expected);
        }
        other => panic!("expected native image, got {other:?}"),
    }
}

#[tokio::test]
async fn native_images_and_notebook_edit_read_journey() {
    let dir = tempfile::tempdir().unwrap();
    let files = ClaudeWorkspaceFiles::new(dir.path()).unwrap();
    for (format, mime) in [
        (image::ImageFormat::Png, "image/png"),
        (image::ImageFormat::Jpeg, "image/jpeg"),
        (image::ImageFormat::Gif, "image/gif"),
        (image::ImageFormat::WebP, "image/webp"),
    ] {
        let bytes = image_bytes(format);
        fs::write(dir.path().join("screenshot"), &bytes).unwrap();
        let result = files
            .execute_output("Read", json!({"file_path":"screenshot"}))
            .await
            .unwrap();
        assert_image(&blocks(result)[0], &bytes, mime);
        assert!(
            files
                .execute("Read", json!({"file_path":"screenshot"}))
                .await
                .unwrap_err()
                .contains("execute_output")
        );
        println!("Read extensionless {mime}: exact bytes preserved in native Claude image block");
    }
    fs::write(dir.path().join("bad.png"), "not an image").unwrap();
    assert!(
        files
            .execute_output("Read", json!({"file_path":"bad.png"}))
            .await
            .unwrap_err()
            .contains("invalid image")
    );
    fs::write(dir.path().join("big.png"), vec![0; 5 * 1024 * 1024 + 1]).unwrap();
    assert!(
        files
            .execute_output("Read", json!({"file_path":"big.png"}))
            .await
            .unwrap_err()
            .contains("5 MiB")
    );
    let png = image_bytes(image::ImageFormat::Png);
    let notebook = json!({"nbformat":4,"nbformat_minor":5,"metadata":{"language_info":{"name":"python"}},"cells":[
        {"id":"intro","cell_type":"markdown","metadata":{},"source":["# Example\n"]},
        {"id":"plot","cell_type":"code","metadata":{"tag":"keep"},"source":["plot()\n"],"execution_count":1,"outputs":[
            {"output_type":"stream","name":"stdout","text":["Rendered\n"]},
            {"output_type":"display_data","data":{"image/png":STANDARD.encode(&png),"text/plain":["A plot"]},"metadata":{}},
            {"output_type":"error","ename":"ExampleError","evalue":"example","traceback":["trace"]}
        ]}
    ]});
    fs::write(
        dir.path().join("note.ipynb"),
        serde_json::to_vec(&notebook).unwrap(),
    )
    .unwrap();
    let edits = ClaudeNotebook::new(dir.path()).unwrap();
    edits
        .execute(
            "NotebookEdit",
            json!({"notebook_path":"note.ipynb","cell_id":"intro","new_source":"# Updated\n"}),
        )
        .await
        .unwrap();
    let updated: Value =
        serde_json::from_slice(&fs::read(dir.path().join("note.ipynb")).unwrap()).unwrap();
    assert_eq!(updated["cells"][1], notebook["cells"][1]);
    let result = files
        .execute_output(
            "Read",
            json!({"file_path":"note.ipynb","offset":2,"limit":1}),
        )
        .await
        .unwrap();
    assert_eq!(result.metadata.as_ref().unwrap()["kind"], "notebook");
    let parts = blocks(result);
    let texts = parts
        .iter()
        .filter_map(|b| match b {
            ToolResultBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>();
    assert!(
        texts.contains("Cell plot (code, index 1)")
            && texts.contains("Rendered")
            && texts.contains("ExampleError: example")
            && !texts.contains("Updated")
    );
    assert_image(
        parts
            .iter()
            .find(|b| matches!(b, ToolResultBlock::Image { .. }))
            .unwrap(),
        &png,
        "image/png",
    );
    let mut invalid = notebook.clone();
    invalid["cells"][1]["outputs"][1]["data"] = json!({"image/jpeg":STANDARD.encode(&png)});
    fs::write(
        dir.path().join("bad.ipynb"),
        serde_json::to_vec(&invalid).unwrap(),
    )
    .unwrap();
    assert!(
        files
            .execute_output("Read", json!({"file_path":"bad.ipynb"}))
            .await
            .unwrap_err()
            .contains("MIME type")
    );
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret.png"), &png).unwrap();
    assert!(
        files
            .execute_output(
                "Read",
                json!({"file_path":outside.path().join("secret.png")})
            )
            .await
            .unwrap_err()
            .contains("outside workspace")
    );
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(outside.path(), dir.path().join("escape")).unwrap();
        assert!(
            files
                .execute_output("Read", json!({"file_path":"escape/secret.png"}))
                .await
                .unwrap_err()
                .contains("symlink escapes")
        );
    }
    fs::write(dir.path().join("CLAUDE.md"), "Notebook workspace guidance").unwrap();
    let edited: Value = serde_json::from_str(&edits.execute("NotebookEdit", json!({"notebook_path":"note.ipynb","cell_id":"intro","new_source":"# Updated again\n"})).await.unwrap()).unwrap();
    assert_eq!(
        edited["project_context"]["excerpts"][0]["text"],
        "Notebook workspace guidance"
    );
    println!(
        "NotebookEdit -> Read: metadata/outputs preserved, cell range and native image verified; malformed/oversized media and path escapes rejected"
    );
}

#[tokio::test]
async fn practical_glob_and_type_search_journey() {
    let dir = tempfile::tempdir().unwrap();
    let files = ClaudeWorkspaceFiles::new(dir.path()).unwrap();
    for (path, content) in [
        ("src/a.rs", "needle\n"),
        ("src/b.ts", "needle\n"),
        ("src/c.py", "needle\n"),
        ("src/d.txt", "needle\n"),
        ("src/binary.rs", "needle\0binary"),
    ] {
        files
            .execute("Write", json!({"file_path":path,"content":content}))
            .await
            .unwrap();
    }
    assert_eq!(
        files
            .execute("Grep", json!({"pattern":"needle","type":"rust"}))
            .await
            .unwrap(),
        "src/a.rs\n"
    );
    assert_eq!(
        files
            .execute("Grep", json!({"pattern":"needle","glob":"**/[ab].{rs,ts}"}))
            .await
            .unwrap(),
        "src/a.rs\nsrc/b.ts\n"
    );
    assert_eq!(
        files
            .execute(
                "Grep",
                json!({"pattern":"needle","type":"ts","glob":"!*.rs"})
            )
            .await
            .unwrap(),
        "src/b.ts\n"
    );
    let found = files
        .execute("Glob", json!({"pattern":"src/[ab].{rs,ts}"}))
        .await
        .unwrap();
    assert!(
        found.contains("src/a.rs") && found.contains("src/b.ts") && !found.contains("src/c.py")
    );
    assert!(
        files
            .execute("Grep", json!({"pattern":"needle","type":"not_a_type"}))
            .await
            .unwrap_err()
            .contains("invalid file type")
    );
    assert!(
        files
            .execute("Grep", json!({"pattern":"needle","glob":"../*"}))
            .await
            .is_err()
    );
    println!(
        "Grep/Glob: ripgrep rust/ts types, brace alternatives, character classes, exclusion, binary filtering and invalid options verified"
    );
}

// Minimal valid PDFs generated locally, then parsed/rendered by real Poppler.
fn pdf(pages: usize) -> Vec<u8> {
    let mut objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        format!(
            "<< /Type /Pages /Count {pages} /Kids [{}] >>",
            (0..pages)
                .map(|n| format!("{} 0 R", 3 + n * 2))
                .collect::<Vec<_>>()
                .join(" ")
        ),
    ];
    for n in 0..pages {
        objects.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents {} 0 R >>",
            4 + n * 2
        ));
        let content = format!(
            "{} 0 0 rg 20 20 160 160 re f\n",
            (n + 1) as f32 / pages as f32
        );
        objects.push(format!(
            "<< /Length {} >>\nstream\n{content}endstream",
            content.len()
        ));
    }
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, object) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for offset in offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    out
}

fn poppler() -> MediaReadOptions {
    MediaReadOptions {
        pdfinfo: std::env::var_os("NANOCODEX_TEST_PDFINFO")
            .map(PathBuf::from)
            .unwrap_or_else(|| "pdfinfo".into()),
        pdftoppm: std::env::var_os("NANOCODEX_TEST_PDFTOPPM")
            .map(PathBuf::from)
            .unwrap_or_else(|| "pdftoppm".into()),
    }
}

#[tokio::test]
#[ignore = "requires real Poppler pdfinfo/pdftoppm; set NANOCODEX_TEST_PDFINFO/PDFTOPPM if not on PATH"]
async fn real_pdf_native_read_journey() {
    let dir = tempfile::tempdir().unwrap();
    let files = ClaudeWorkspaceFiles::new(dir.path())
        .unwrap()
        .with_media_options(poppler());
    fs::write(dir.path().join("small.pdf"), pdf(2)).unwrap();
    fs::write(dir.path().join("large.pdf"), pdf(22)).unwrap();
    let result = files
        .execute_output("Read", json!({"file_path":"small.pdf"}))
        .await
        .unwrap();
    assert_eq!(result.metadata.as_ref().unwrap()["total_pages"], 2);
    let parts = blocks(result);
    assert_eq!(parts.len(), 4);
    for part in [&parts[1], &parts[3]] {
        match part {
            ToolResultBlock::Image {
                source: ImageSource::Base64 { media_type, data },
            } => {
                assert_eq!(media_type, "image/png");
                let image = image::load_from_memory(&STANDARD.decode(data).unwrap()).unwrap();
                assert_eq!((image.width(), image.height()), (1600, 1600));
            }
            _ => panic!("page missing native image"),
        }
    }
    assert_ne!(
        serde_json::to_value(&parts[1]).unwrap(),
        serde_json::to_value(&parts[3]).unwrap()
    );
    assert!(
        files
            .execute_output("Read", json!({"file_path":"large.pdf"}))
            .await
            .unwrap_err()
            .contains("specify pages")
    );
    let selected = files
        .execute_output("Read", json!({"file_path":"large.pdf","pages":"12-13"}))
        .await
        .unwrap();
    assert_eq!(selected.metadata.as_ref().unwrap()["first_page"], 12);
    assert_eq!(selected.metadata.as_ref().unwrap()["last_page"], 13);
    for pages in ["0", "23", "3-2", "1-21", "1-2-3"] {
        assert!(
            files
                .execute_output("Read", json!({"file_path":"large.pdf","pages":pages}))
                .await
                .is_err()
        );
    }
    fs::write(dir.path().join("broken.pdf"), b"%PDF-1.4\nbroken").unwrap();
    assert!(
        files
            .execute_output("Read", json!({"file_path":"broken.pdf"}))
            .await
            .unwrap_err()
            .contains("helper failed")
    );
    let missing = ClaudeWorkspaceFiles::new(dir.path())
        .unwrap()
        .with_media_options(MediaReadOptions {
            pdfinfo: dir.path().join("missing-helper"),
            pdftoppm: "pdftoppm".into(),
        });
    assert!(
        missing
            .execute_output("Read", json!({"file_path":"small.pdf"}))
            .await
            .unwrap_err()
            .contains("install Poppler")
    );
    println!(
        "Real Poppler: distinct native PNG blocks at 1600x1600 for both pages; explicit 12-13 selection; >10-page requirement, invalid/over-20 ranges, malformed PDF and unavailable helper errors verified"
    );
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "requires real Poppler and exercises the actual 30-second helper deadline"]
async fn delayed_poppler_timeout_cleanup_and_recovery_journey() {
    use std::{
        os::unix::fs::PermissionsExt,
        time::{Duration, Instant},
    };
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("page.pdf"), pdf(1)).unwrap();
    let options = poppler();
    let quote = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
    let marker = dir.path().join("helper-state");
    let wrapper = dir.path().join("delayed-pdfinfo");
    // Wrap the actual external helper with controlled latency. The adapter itself
    // and its timeout/cleanup implementation are never stubbed or mocked.
    let script = format!(
        "#!/bin/sh\nsleep 60 &\ndelayed=$!\nprintf '%s\\n%s\\n' \"$1\" \"$delayed\" > {}\nwait \"$delayed\"\nexec {} \"$@\"\n",
        quote(marker.to_str().unwrap()),
        quote(options.pdfinfo.to_str().unwrap())
    );
    fs::write(&wrapper, script).unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700)).unwrap();
    let delayed = ClaudeWorkspaceFiles::new(dir.path())
        .unwrap()
        .with_media_options(MediaReadOptions {
            pdfinfo: wrapper,
            pdftoppm: options.pdftoppm.clone(),
        });
    let started = Instant::now();
    let error = delayed
        .execute_output("Read", json!({"file_path":"page.pdf"}))
        .await
        .unwrap_err();
    let elapsed = started.elapsed();
    assert!(error.contains("exceeded 30 seconds"), "{error}");
    assert!(
        elapsed >= Duration::from_secs(29) && elapsed < Duration::from_secs(40),
        "deadline elapsed {elapsed:?}"
    );
    let state = fs::read_to_string(marker).unwrap();
    let mut state = state.lines();
    let snapshot = PathBuf::from(state.next().unwrap());
    assert!(
        !snapshot.exists(),
        "private snapshot was not removed: {snapshot:?}"
    );
    let pid = state.next().unwrap().parse::<i32>().unwrap();
    #[cfg(target_os = "linux")]
    {
        // Linux may keep a killed orphan as a zombie until container PID 1 reaps it.
        // A zombie cannot execute or hold pipes; absence and Z are both stopped.
        let stat_path = format!("/proc/{pid}/stat");
        let stopped = || {
            fs::read_to_string(&stat_path)
                .map(|s| {
                    s.split_once(") ")
                        .is_some_and(|(_, rest)| rest.starts_with('Z'))
                })
                .unwrap_or(true)
        };
        for _ in 0..100 {
            if stopped() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            stopped(),
            "helper child {pid} is still executing after timeout"
        );
    }
    let files = ClaudeWorkspaceFiles::new(dir.path())
        .unwrap()
        .with_media_options(options);
    assert_eq!(
        blocks(
            files
                .execute_output("Read", json!({"file_path":"page.pdf"}))
                .await
                .unwrap()
        )
        .len(),
        2
    );
    println!(
        "Delayed real Poppler: returned {error:?} after {elapsed:?}; snapshot removed, inherited-pipe child stopped, next PDF read recovered"
    );
}
