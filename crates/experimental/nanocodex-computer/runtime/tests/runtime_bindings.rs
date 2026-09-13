use serde_json::{Value, json};
use skyre::runtime::Host;
use std::{
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};
#[test]
fn parser_backed_cells_match_installed_binding_oracle() {
    let oracle: Value =
        serde_json::from_str(include_str!("oracles/kernel_bindings_2026-09-07.json")).unwrap();
    compare_binding_oracle(&oracle);
}
#[test]
fn nested_var_classes_destructuring_and_loop_failures_match_installed_kernel() {
    compare_binding_oracle(
        &serde_json::from_str(include_str!(
            "oracles/kernel_bindings_extended_2026-09-07.json"
        ))
        .unwrap(),
    );
}
fn compare_binding_oracle(oracle: &Value) {
    let mut differences = vec![];
    for scenario in oracle["scenarios"].as_array().unwrap() {
        let mut host = Host::with_dispatch(
            |method, _| {
                assert_eq!(method, "sky.setup");
                Ok(json!({"target":"mac"}))
            },
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        for case in scenario["cells"].as_array().unwrap() {
            let result = host
                .evaluate(case["code"].as_str().unwrap(), Duration::from_secs(3))
                .unwrap();
            let text = result["outputs"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|output| output["named"] != true && output["kind"] != "image")
                .map(|output| output["value"].as_str().unwrap())
                .collect::<String>();
            let mut content = vec![];
            if !text.is_empty() {
                content.push(json!({"type":"text","text":text}));
            }
            if result.get("error").is_some() {
                content.push(json!({"type":"text","text":result["exceptionMessage"].as_str().or_else(||result["error"]["message"].as_str()).unwrap()}));
            }
            if content.is_empty() {
                content.push(json!({"type":"text","text":""}));
            }
            let actual = json!({"content":content,"isError":result.get("error").is_some()});
            if actual != case["result"] {
                differences.push(json!({"scenario":scenario["name"],"code":case["code"],"expected":case["result"],"actual":actual}));
            }
        }
    }
    assert!(
        differences.is_empty(),
        "{}",
        serde_json::to_string_pretty(&differences).unwrap()
    );
}

#[test]
fn adjacent_declarations_and_loops_preserve_statement_boundaries() {
    let mut host = Host::with_dispatch(
        |_, _| Ok(json!({"target":"mac"})),
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    for code in [
        "class Box{value(){return 5}}new Box().value()",
        "function f(){return 5}f()",
        "for(var i of [4,5]){}i",
        "for(var key in {a:1}){}key.length+4",
    ] {
        let result = host.evaluate(code, Duration::from_secs(3)).unwrap();
        assert_eq!(result["value"], json!(5), "{code}: {result}");
        assert!(result.get("error").is_none(), "{result}");
    }
}
