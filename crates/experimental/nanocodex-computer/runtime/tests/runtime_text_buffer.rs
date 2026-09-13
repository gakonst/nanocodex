use serde_json::{Value, json};
use skyre::runtime::Host;
use std::{
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};
fn compare_oracle(category: &str, source: &str) {
    let oracle: Value = serde_json::from_str(source).unwrap();
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
            .evaluate(
                group["code"].as_str().unwrap(),
                Duration::from_secs(
                    if group["cases"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|case| case.as_str().unwrap().starts_with("overlap/"))
                    {
                        // This group checks 54 large search results, not a
                        // timing contract. QuickJS alone takes about 25s;
                        // leave headroom for concurrent test/build activity.
                        60
                    } else {
                        5
                    },
                ),
            )
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
            if &actual[index] != expected {
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
fn streaming_text_decoder_matches_installed_labels_errors_and_chunk_boundaries() {
    compare_oracle(
        "decoder",
        include_str!("oracles/runtime_text_buffer_2026-09-07.json"),
    );
}
#[test]
fn buffer_search_swap_fill_write_match_installed_mutations_and_failures() {
    compare_oracle(
        "buffer",
        include_str!("oracles/runtime_text_buffer_2026-09-07.json"),
    );
}
#[test]
fn decoder_all_byte_tables_stream_reuse_and_buffer_coercions_match_installed_kernel() {
    compare_oracle(
        "",
        include_str!("oracles/runtime_text_buffer_extended_2026-09-07.json"),
    );
}
#[test]
fn randomized_decoder_streams_and_binary_searches_match_installed_kernel() {
    compare_oracle(
        "",
        include_str!("oracles/runtime_text_buffer_randomized_2026-09-07.json"),
    );
}

#[test]
fn long_overlapping_buffer_searches_match_installed_kernel() {
    compare_oracle(
        "",
        include_str!("oracles/runtime_text_buffer_overlap_2026-09-07.json"),
    );
}
