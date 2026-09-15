use serde_json::{Value, json};
use skyre::runtime::{Host, HostOptions};
use std::{
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

#[test]
fn url_search_params_matches_installed_kernel() {
    let oracle: Value =
        serde_json::from_str(include_str!("oracles/url_search_params.json")).unwrap();
    let mut host = Host::with_dispatch_options(
        |method, _| match method {
            "sky.setup" => Ok(json!({"target":"mac"})),
            other => panic!("Unexpected provider operation: {other}"),
        },
        Arc::new(AtomicBool::new(false)),
        HostOptions::default(),
    )
    .unwrap();
    for case in oracle["cases"].as_array().unwrap() {
        let result = host
            .evaluate(case["code"].as_str().unwrap(), Duration::from_secs(5))
            .unwrap();
        assert!(result.get("error").is_none(), "{}: {result}", case["name"]);
        let output = result["outputs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|value| value["channel"] == "output")
            .unwrap();
        let value: Value = serde_json::from_str(output["value"].as_str().unwrap()).unwrap();
        assert_eq!(value, case["expected"], "{}", case["name"]);
    }
}
