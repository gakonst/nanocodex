use serde_json::{Value, json};
use skyre::runtime::Host;
use std::{
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};
fn compare(category: &str) {
    compare_oracle(
        category,
        include_str!("oracles/runtime_numeric_url_2026-09-07.json"),
    );
}
fn compare_oracle(category: &str, source: &str) {
    let oracle: Value = serde_json::from_str(source).unwrap();
    // Empty pathToFileURL input records the capture process's working directory.
    // Only rebase that exact directory prefix for pathToFileURL expectations, so
    // the pinned relative-path cases also work in a relocated checkout.
    let captured_directory = oracle["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|group| {
            group["cases"]
                .as_array()
                .unwrap()
                .iter()
                .enumerate()
                .find_map(|(index, case)| {
                    (case == "pathToFileURL/false/")
                        .then(|| group["expected"][index]["value"].as_str())
                        .flatten()
                })
        });
    let current_directory = url::Url::from_directory_path(std::env::current_dir().unwrap())
        .unwrap()
        .to_string();
    let current_directory = current_directory.trim_end_matches('/');
    let mut differences = vec![];
    for group in oracle["groups"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|group| group["name"].as_str().unwrap().starts_with(category))
    {
        let mut host = Host::with_dispatch(
            |method, _| {
                assert_eq!(method, "sky.setup");
                Ok(json!({"target":"mac"}))
            },
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        let result = host
            .evaluate(group["code"].as_str().unwrap(), Duration::from_secs(5))
            .unwrap();
        assert!(
            result.get("error").is_none(),
            "{}: {:?}",
            group["name"],
            result.get("error")
        );
        let output = result["outputs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|output| output["named"] != true && output["channel"] == "output")
            .unwrap();
        let actual: Value = serde_json::from_str(output["value"].as_str().unwrap()).unwrap();
        for (index, expected) in group["expected"].as_array().unwrap().iter().enumerate() {
            // Node includes its actual platform in this error, independently of
            // the `windows` option. The retained oracle was captured on Darwin.
            let mut expected = expected.clone();
            if group["cases"][index]
                .as_str()
                .unwrap()
                .starts_with("pathToFileURL/")
                && let Some(captured_directory) = captured_directory
                && let Some(value) = expected["value"].as_str()
                && let Some(suffix) = value.strip_prefix(captured_directory)
                && (suffix.is_empty() || suffix.starts_with('/'))
            {
                expected["value"] = format!("{current_directory}{suffix}").into();
            }
            if expected["error"]["code"] == "ERR_INVALID_FILE_URL_HOST"
                && let Some(message) = expected["error"]["message"].as_str()
            {
                let platform = if cfg!(target_os = "macos") {
                    "darwin"
                } else if cfg!(target_os = "windows") {
                    "win32"
                } else {
                    std::env::consts::OS
                };
                expected["error"]["message"] = message
                    .replace(" on darwin", &format!(" on {platform}"))
                    .into();
            }
            if actual[index] != expected {
                differences.push(json!({"case":group["cases"][index],"expected":expected,"actual":actual[index]}));
            }
        }
    }
    assert!(
        differences.is_empty(),
        "{} mismatches: {}",
        differences.len(),
        serde_json::to_string_pretty(&differences.iter().take(60).collect::<Vec<_>>()).unwrap()
    );
}
#[test]
fn numeric_buffer_methods_match_1118_installed_kernel_cases() {
    compare("numeric");
}
#[test]
fn node_url_matches_134_installed_kernel_cases() {
    compare("url");
}

#[test]
fn numeric_and_url_extended_coercion_metadata_and_legacy_cases_match_installed_kernel() {
    compare_oracle(
        "",
        include_str!("oracles/runtime_numeric_url_extended_2026-09-07.json"),
    );
}

#[test]
fn unicode_url_format_preserves_matching_userinfo_and_query_text() {
    compare_oracle(
        "",
        include_str!("oracles/runtime_numeric_url_authority_2026-09-07.json"),
    );
}

#[test]
fn float_writers_preserve_nan_payloads_and_canonicalize_undefined_coercion() {
    compare_oracle(
        "",
        include_str!("oracles/runtime_buffer_nan_2026-09-07.json"),
    );
}
