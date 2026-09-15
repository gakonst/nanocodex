//! Benign own-property effects in the existing pre-bootstrap test seam.
//! This is not a public installed first-call mode or a provider-policy test.
use super::*;

fn publication_cases(ids: &[&str]) {
    let corpus: Value = serde_json::from_str(include_str!(
        "../tests/oracles/cua_setup_publication_cases.json"
    ))
    .unwrap();
    for runtime in RuntimeBackend::available() {
        for id in ids {
            let case = corpus["cases"]
                .as_array()
                .unwrap()
                .iter()
                .find(|case| case["id"] == *id)
                .unwrap();
            let (mut host, calls) = host(runtime, false);
            let code = format!(
                "var publicationCase = {};\n{}",
                serde_json::to_string(id).unwrap(),
                include_str!("../tests/cua_setup_publication_case.js")
            );
            let mut actual = prelude_free(&mut host, &code);
            println!("publication {runtime:?} {id}: {actual}");
            let mut expected = case["expected"].clone();
            if *id == "facade-nonextensible" {
                // Preserve raw engine diagnostics above. The native engines
                // have different standard TypeError text; the exact install
                // ordering, error class and complete observed state still match.
                assert!(!actual["error"]["message"].as_str().unwrap().is_empty());
                actual["error"].as_object_mut().unwrap().remove("message");
                expected["error"].as_object_mut().unwrap().remove("message");
            }
            assert_eq!(actual, expected, "{runtime:?}: {id}");
            assert_eq!(&*calls.borrow(), &["sky.setup"], "{runtime:?}: {id}");
        }
    }
}

#[test]
fn first_setup_publishes_agent_before_methods_and_preserves_false_results() {
    publication_cases(&[
        "ordinary",
        "agent-observing-setter",
        "agent-readonly-data",
        "agent-accessor-without-setter",
        "disabled-browser-agent-throwing-setter",
    ]);
}

#[test]
fn first_setup_publication_and_target_errors_preserve_assignment_effects() {
    publication_cases(&[
        "agent-throwing-setter",
        "facade-nonextensible",
        "facade-getState-throwing-setter",
        "facade-getBrowser-observing-setter",
    ]);
}

#[test]
fn first_setup_publishes_agent_before_temporary_assignment() {
    let corpus: Value = serde_json::from_str(include_str!(
        "../tests/oracles/cua_setup_assignment_cases.json"
    ))
    .unwrap();
    assert_eq!(corpus["cases"].as_array().unwrap().len(), 2);
    for runtime in RuntimeBackend::available() {
        for case in corpus["cases"].as_array().unwrap() {
            let id = case["id"].as_str().unwrap();
            let (mut host, calls) = host(runtime, false);
            let code = format!(
                "var assignmentCase = {};\n{}",
                serde_json::to_string(id).unwrap(),
                include_str!("../tests/cua_setup_assignment_case.js")
            );
            let actual = prelude_free(&mut host, &code);
            println!("assignment {runtime:?} {id}: {actual}");
            assert_eq!(actual, case["expected"], "{runtime:?}: {id}");
            assert_eq!(&*calls.borrow(), &["sky.setup"], "{runtime:?}: {id}");
        }
    }
}
