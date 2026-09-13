use serde_json::{Value, json};
use skyre::runtime::{Host, HostOptions, RuntimeBackend};
use std::{
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

#[test]
fn querystring_matches_installed_kernel() {
    let oracle: Value =
        serde_json::from_str(include_str!("oracles/runtime_querystring.json")).unwrap();
    let code = concat!(
        include_str!("runtime_querystring_cases.js"),
        "\nnodeRepl.write(JSON.stringify(await querystringCases()));"
    );
    for runtime in RuntimeBackend::available() {
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
        let result = host.evaluate(code, Duration::from_secs(30)).unwrap();
        assert!(
            result.get("error").is_none(),
            "{runtime:?}: {}",
            result["error"]
        );
        let output = result["outputs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|value| value["channel"] == "output")
            .unwrap();
        let actual: Value = serde_json::from_str(output["value"].as_str().unwrap()).unwrap();
        let expected = oracle["expected"].as_array().unwrap();
        assert_eq!(actual.as_array().unwrap().len(), expected.len());
        let differences: Vec<_> = expected
            .iter()
            .zip(actual.as_array().unwrap())
            .filter(|(expected, actual)| expected != actual)
            .map(|(expected, actual)| json!({"expected":expected, "actual":actual}))
            .collect();
        assert!(
            differences.is_empty(),
            "{runtime:?}: {} differing cases: {}",
            differences.len(),
            serde_json::to_string_pretty(&differences.iter().take(12).collect::<Vec<_>>()).unwrap()
        );
    }
}

#[test]
fn querystring_hooks_are_worker_owned_resettable_and_bounded() {
    use skyre::worker::{Event, Worker};
    fn evaluate(worker: &mut Worker, code: &str) -> Value {
        worker.start(code, Duration::from_secs(10)).unwrap();
        loop {
            match worker
                .event(Duration::from_secs(15))
                .unwrap()
                .expect("worker progress")
            {
                Event::Call { method, reply, .. } => {
                    assert_eq!(method, "sky.setup");
                    reply.send(Ok(json!({"target":"mac"}))).unwrap();
                }
                Event::Done { result, .. } => {
                    let result = result.unwrap();
                    assert!(result.get("error").is_none(), "{}", result["error"]);
                    return result["value"].clone();
                }
            }
        }
    }
    for runtime in RuntimeBackend::available() {
        let options = HostOptions {
            runtime,
            ..Default::default()
        };
        let mut first = Worker::with_options_and_executable(
            options.clone(),
            env!("CARGO_BIN_EXE_nanocodex-computer").into(),
        );
        let mut second = Worker::with_options_and_executable(
            options,
            env!("CARGO_BIN_EXE_nanocodex-computer").into(),
        );
        let setup = "let q = (await import('node:querystring')).default; q.stringify({x:'a b'})";
        assert_eq!(evaluate(&mut first, setup), "x=a%20b");
        assert_eq!(evaluate(&mut second, setup), "x=a%20b");
        assert_eq!(
            evaluate(
                &mut first,
                "q.escape = text => '['+text+']'; (await import('querystring')).default.stringify({x:'a b'})"
            ),
            "[x]=[a b]"
        );
        assert_eq!(evaluate(&mut second, "q.stringify({x:'a b'})"), "x=a%20b");
        let bounds = r#"['parse','escape','unescape','unescapeBuffer'].map(name=>{
            try { q[name]('x'.repeat(8*1024*1024+1)); return 'unbounded'; }
            catch(error) { return error.code; }
        })"#;
        assert_eq!(
            evaluate(&mut second, bounds),
            json!([
                "ERR_OUT_OF_RANGE",
                "ERR_OUT_OF_RANGE",
                "ERR_OUT_OF_RANGE",
                "ERR_OUT_OF_RANGE"
            ])
        );
        assert_eq!(
            evaluate(&mut second, "q.stringify(q.parse('x=a+b&x=%E2%82%AC'))"),
            "x=a%20b&x=%E2%82%AC"
        );
        first.reset().unwrap();
        assert_eq!(evaluate(&mut first, "typeof q"), "undefined");
        assert_eq!(evaluate(&mut first, setup), "x=a%20b");
        assert_eq!(
            evaluate(&mut second, "q.stringify({still:'owned'})"),
            "still=owned"
        );
    }
}
