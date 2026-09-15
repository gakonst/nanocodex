use serde_json::{Value, json};
use skyre::runtime::{Host, HostOptions, RuntimeBackend};
use std::{
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};
fn host() -> Host {
    Host::with_dispatch(
        |method, _| {
            assert_eq!(method, "sky.setup");
            Ok(json!({"target":"mac"}))
        },
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap()
}
fn oracle() -> Value {
    serde_json::from_str(include_str!("oracles/runtime_modules_2026-09-07.json")).unwrap()
}
#[test]
fn path_posix_and_win32_match_352_installed_node_cases() {
    let mut host = host();
    let mut differences = vec![];
    for case in oracle()["pathCases"].as_array().unwrap() {
        let code = format!(
            "(await import('node:path'))[{}][{}](...{})",
            case["flavor"], case["method"], case["args"]
        );
        let result = host.evaluate(&code, Duration::from_secs(3)).unwrap();
        if result.get("error").is_some() || result["value"] != case["result"] {
            differences.push(
                json!({"case":case,"actual":result.get("error").unwrap_or(&result["value"])}),
            );
        }
    }
    assert!(
        differences.is_empty(),
        "{}",
        serde_json::to_string_pretty(&differences).unwrap()
    );
}
#[test]
fn fs_promises_performs_real_owned_file_lifecycle_matching_installed_node() {
    let directory = tempfile::tempdir().unwrap();
    let oracle = oracle();
    let code = format!(
        "const root={};\n{}",
        json!(directory.path().to_str().unwrap()),
        oracle["filesystem"]["code"].as_str().unwrap()
    );
    let result = host().evaluate(&code, Duration::from_secs(5)).unwrap();
    assert!(result.get("error").is_none(), "{result}");
    assert_eq!(result["value"], oracle["filesystem"]["result"]);
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}
#[test]
fn filesystem_bytes_file_urls_and_bounds_are_real() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("image β.png");
    std::fs::write(&file, b"\x89PNG\r\n\x1a\n").unwrap();
    let url = url::Url::from_file_path(&file).unwrap().to_string();
    let mut host = host();
    let code = format!(
        "const fs=await import('node:fs/promises');const image=await fs.readFile(new URL({}));await nodeRepl.emitImage(image);[Buffer.isBuffer(image),image.toString('hex')];",
        json!(url)
    );
    let result = host.evaluate(&code, Duration::from_secs(3)).unwrap();
    assert_eq!(
        result["value"],
        json!([true, "89504e470d0a1a0a"]),
        "{result}"
    );
    assert!(
        result["outputs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|output| output["kind"] == "image")
    );
    std::fs::File::create(&file)
        .unwrap()
        .set_len(8 * 1024 * 1024 + 1)
        .unwrap();
    let result = host
        .evaluate(
            &format!(
                "try{{await fs.readFile({})}}catch(error){{nodeRepl.write(error.message)}}",
                json!(file.to_str().unwrap())
            ),
            Duration::from_secs(3),
        )
        .unwrap();
    assert!(result["outputs"].to_string().contains("exceeds 8 MiB"));
}
#[test]
fn file_handles_links_copy_identity_and_errors_match_installed_node() {
    let directory = tempfile::tempdir().unwrap();
    let oracle: Value = serde_json::from_str(include_str!(
        "oracles/runtime_modules_edges_2026-09-07.json"
    ))
    .unwrap();
    let code = format!(
        "const root={};\n{}",
        json!(directory.path().to_str().unwrap()),
        oracle["filesystem"]["code"].as_str().unwrap()
    );
    let result = host().evaluate(&code, Duration::from_secs(5)).unwrap();
    assert!(result.get("error").is_none(), "{result}");
    assert_eq!(result["value"], oracle["filesystem"]["result"]);
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[test]
fn file_handles_publish_only_regular_files() {
    for runtime in RuntimeBackend::available() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("owned.txt");
        let mut host = Host::with_dispatch_options(
            |method, _| {
                assert_eq!(method, "sky.setup");
                Ok(json!({"target":"mac"}))
            },
            Arc::new(AtomicBool::new(false)),
            HostOptions {
                runtime,
                ..Default::default()
            },
        )
        .unwrap();
        let code = format!(
            r#"const fs=await import('node:fs/promises');
            let rejected=false;
            try {{ const handle=await fs.open({},'r');await handle.close(); }}
            catch(error) {{ rejected=true; }}
            const file=await fs.open({},'w+');
            await file.writeFile('owned regular bytes');
            const regular=(await file.stat()).isFile();
            await file.close();
            [rejected,regular,(await fs.stat({})).isDirectory()];"#,
            json!(directory.path().to_str().unwrap()),
            json!(file.to_str().unwrap()),
            json!(directory.path().to_str().unwrap()),
        );
        let result = host.evaluate(&code, Duration::from_secs(5)).unwrap();
        assert!(result.get("error").is_none(), "{runtime:?}: {result}");
        assert_eq!(result["value"], json!([true, true, true]), "{runtime:?}");
        assert_eq!(std::fs::read(file).unwrap(), b"owned regular bytes");
    }
}
#[test]
fn inspected_buffers_match_installed_node() {
    let oracle: Value = serde_json::from_str(include_str!(
        "oracles/runtime_modules_edges_2026-09-07.json"
    ))
    .unwrap();
    let mut host = host();
    for case in oracle["buffers"].as_array().unwrap() {
        let result = host
            .evaluate(
                &format!("nodeRepl.write(Buffer.alloc({}))", case["size"]),
                Duration::from_secs(3),
            )
            .unwrap();
        let text = result["outputs"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|value| value["value"].as_str())
            .filter(|text| text.starts_with("<Buffer"))
            .collect::<String>();
        assert_eq!(text, case["output"].as_str().unwrap(), "{result}");
    }
}
