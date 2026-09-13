//! Actual native Worker control transport. The fixture uses an existing info
//! callback to inspect admission through Engine; this is not a browser journey
//! or a public WebMCP capability advertisement.
use serde_json::{Value, json};
use skyre::{
    engine::Engine,
    fixture::Fixture,
    runtime::{HostOptions, RuntimeBackend},
    worker::{Event, Worker},
};
use std::{
    io::ErrorKind,
    net::TcpListener,
    time::{Duration, Instant},
};

fn evaluate(worker: &mut Worker, engine: &mut Engine, expected: &str) {
    worker
        .start(
            "await agent.browsers.get('owned'); 42",
            Duration::from_secs(5),
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(12);
    let mut observed = 0;
    loop {
        assert!(Instant::now() < deadline, "bounded native fixture progress");
        match worker
            .event(deadline.saturating_duration_since(Instant::now()))
            .unwrap()
            .expect("native progress")
        {
            Event::Call {
                method,
                args,
                control,
                reply,
            } => {
                control.execution_validity().unwrap().validate().unwrap();
                let response = match method.as_str() {
                    "sky.setup" => json!({"target":"mac"}),
                    "browser.info" => {
                        assert!(
                            worker
                                .set_request_meta(Some(
                                    json!({"x-codex-turn-metadata":{"model":"GPT-6-ASTRA"}})
                                ))
                                .is_err()
                        );
                        let error = engine
                            .execute_from_js_controlled(
                                "browser.webmcp_list_tools",
                                &json!({"browser_id":"owned","tab_id":"t","model":"GPT-6-ASTRA"}),
                                &control,
                            )
                            .unwrap_err();
                        assert_eq!(error.message, expected);
                        // A normal suspension does not replace the metadata owner.
                        control.suspend().unwrap().resume().unwrap();
                        assert_eq!(
                            engine
                                .execute_from_js_controlled(
                                    "browser.webmcp_list",
                                    &json!({"browser":"owned","tab":"t"}),
                                    &control
                                )
                                .unwrap_err()
                                .message,
                            expected
                        );
                        observed += 1;
                        json!({"id":args["browser"],"type":"cdp"})
                    }
                    _ => panic!("Unexpected fixture callback {method}"),
                };
                reply.send(Ok(response)).unwrap();
            }
            Event::Done { result, .. } => {
                let result = result.unwrap();
                assert_eq!(result["value"], 42, "{result}");
                assert_eq!(observed, 1);
                return;
            }
        }
    }
}

#[test]
fn native_worker_activation_metadata_survives_wire_updates_and_reset() {
    for runtime in RuntimeBackend::available() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut engine = Engine::new(Box::new(Fixture::default()));
        engine
            .browsers
            .register("owned", &format!("ws://{}", listener.local_addr().unwrap()))
            .unwrap();
        let mut worker = Worker::with_options_and_executable(
            HostOptions {
                runtime,
                request_meta: Some(json!({"x-codex-turn-metadata":{"model":"GPT-6-LUNA"}})),
                ..Default::default()
            },
            env!("CARGO_BIN_EXE_nanocodex-computer").into(),
        );
        evaluate(
            &mut worker,
            &mut engine,
            "gpt-6-luna does not support command \"webmcp_list_tools\".",
        );
        worker
            .set_request_meta(Some(
                json!({"x-codex-turn-metadata":"{\"model\":\"İΣ-LUNA\"}"}),
            ))
            .unwrap();
        evaluate(
            &mut worker,
            &mut engine,
            "i̇ς-luna does not support command \"webmcp_list_tools\".",
        );
        worker.reset().unwrap();
        evaluate(
            &mut worker,
            &mut engine,
            "i̇ς-luna does not support command \"webmcp_list_tools\".",
        );
        worker
            .set_request_meta(Some(
                json!({"x-codex-turn-metadata":{"model":"GPT-6-ASTRA"}}),
            ))
            .unwrap();
        evaluate(
            &mut worker,
            &mut engine,
            "owned does not support command \"webmcp_list_tools\".",
        );
        worker.set_request_meta(None::<Value>).unwrap();
        evaluate(
            &mut worker,
            &mut engine,
            "owned does not support command \"webmcp_list_tools\".",
        );
        worker
            .set_request_meta(Some(
                json!({"x-codex-turn-metadata":{"model":"x".repeat(65536)+"-LUNA"}}),
            ))
            .unwrap();
        evaluate(
            &mut worker,
            &mut engine,
            "Native WebMCP model projection exceeds limit",
        );
        worker.reset().unwrap();
        evaluate(
            &mut worker,
            &mut engine,
            "Native WebMCP model projection exceeds limit",
        );
        assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
    }
}
