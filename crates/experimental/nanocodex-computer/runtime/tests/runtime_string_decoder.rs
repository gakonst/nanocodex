use serde_json::{Value, json};
use skyre::runtime::{Host, HostOptions, RuntimeBackend};
use std::{
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

#[test]
fn incremental_string_decoder_matches_installed_kernel() {
    let oracle: Value =
        serde_json::from_str(include_str!("oracles/runtime_string_decoder.json")).unwrap();
    let code = concat!(
        include_str!("runtime_string_decoder_cases.js"),
        "\nnodeRepl.write(JSON.stringify(await stringDecoderCases()));"
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
        assert!(result.get("error").is_none(), "{runtime:?}: {result}");
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
fn string_decoder_bounds_preserve_pending_input_and_workers_reset_independently() {
    use skyre::worker::{Event, Worker};
    fn evaluate(worker: &mut Worker, code: &str) -> Value {
        worker.start(code, Duration::from_secs(5)).unwrap();
        loop {
            match worker
                .event(Duration::from_secs(10))
                .unwrap()
                .expect("worker progress")
            {
                Event::Call { method, reply, .. } => {
                    assert_eq!(method, "sky.setup");
                    reply.send(Ok(json!({"target":"mac"}))).unwrap();
                }
                Event::Done { result, .. } => {
                    let result = result.unwrap();
                    assert!(result.get("error").is_none(), "{result}");
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
        let setup = "let {StringDecoder} = await import('string_decoder'); let decoder = new StringDecoder(); decoder.write(new Uint8Array([226])); decoder.lastNeed";
        assert_eq!(evaluate(&mut first, setup), 2);
        assert_eq!(evaluate(&mut second, setup), 2);
        let bounded = r#"let bound; try {decoder.write(new Uint8Array(8*1024*1024+1))} catch(error) {bound=error.code}
            [bound,decoder.lastNeed,decoder.write(new Uint8Array([130,172])),decoder.end()]"#;
        assert_eq!(
            evaluate(&mut first, bounded),
            json!(["ERR_OUT_OF_RANGE", 2, "€", ""])
        );
        first.reset().unwrap();
        assert_eq!(evaluate(&mut first, "typeof decoder"), "undefined");
        assert_eq!(evaluate(&mut second, "decoder.end()"), "�");
        let corrupt = r#"const {StringDecoder}=await import('node:string_decoder');const d=new StringDecoder();
            d[Object.getOwnPropertySymbols(d)[0]][4]=255;
            let failure;try{d.write(new Uint8Array([65]))}catch(error){failure=error.code} failure"#;
        assert_eq!(evaluate(&mut first, corrupt), "ERR_INVALID_STATE");
        assert_eq!(
            evaluate(
                &mut first,
                "new StringDecoder().write(new Uint8Array([65]))"
            ),
            "A"
        );
    }
}
