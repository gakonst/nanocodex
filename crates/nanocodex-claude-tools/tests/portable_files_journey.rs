//! Public adapter integration: real filesystem vs serialized async-host plans.
//! The WASM/Worker transport journey is owned by js/managed's public Worker suite.
use nanocodex_claude_tools::{ClaudeNotebook, ClaudeWorkspaceFiles, portable_plan};
use serde_json::{Value, json};
use std::fs;

fn plan(name: &str, input: Value, files: Value) -> Result<portable_plan::Plan, String> {
    let wire = json!({"root":"/workspace","name":name,"input":input,"files":files});
    portable_plan::plan(serde_json::from_str(&wire.to_string()).unwrap())
}
#[tokio::test]
async fn native_and_portable_file_journey() {
    let dir = tempfile::tempdir().unwrap();
    let native = ClaudeWorkspaceFiles::new(dir.path()).unwrap();
    let initial = "alpha\nrepeat\nrepeat\nΩ end\n";
    let input = json!({"file_path":"nested/a.rs","content":initial});
    let write = plan("Write", input.clone(), json!([])).unwrap();
    let output = native
        .execute_output_with_context("Write", input, false)
        .await
        .unwrap();
    let nanocodex_claude_tools::ToolContent::Text(text) = output.content else {
        panic!("expected text")
    };
    assert_eq!(write.output, text);
    let mut files = json!([{"path":"nested/a.rs","content":initial}]);
    for input in [
        json!({"file_path":"nested/a.rs","old_string":"repeat","new_string":"replaced"}),
        json!({"file_path":"nested/a.rs","old_string":"missing","new_string":"bad"}),
        json!({"file_path":"../escape","old_string":"a","new_string":"bad"}),
    ] {
        assert!(plan("Edit", input.clone(), files.clone()).is_err());
        assert!(native.execute("Edit", input.clone()).await.is_err());
        assert_eq!(
            fs::read_to_string(dir.path().join("nested/a.rs")).unwrap(),
            initial
        );
        println!("Rejected edit without mutation: {input}");
    }
    let edit = json!({"file_path":"nested/a.rs","old_string":"repeat","new_string":"βeta","replace_all":true});
    let result = plan("Edit", edit.clone(), files.clone()).unwrap();
    native.execute("Edit", edit).await.unwrap();
    assert_eq!(result.mutations[0].before.as_deref(), Some(initial));
    assert_eq!(
        result.mutations[0].content,
        fs::read_to_string(dir.path().join("nested/a.rs")).unwrap()
    );
    files[0]["content"] = json!(result.mutations[0].content);
    for (name, input, expected) in [
        (
            "Read",
            json!({"file_path":"nested/a.rs","offset":2,"limit":2}),
            "2\tβeta\n3\tβeta\n",
        ),
        ("Glob", json!({"pattern":"**/*.{rs,ts}"}), "nested/a.rs\n"),
        (
            "Grep",
            json!({"pattern":"βeta","type":"rust","output_mode":"count"}),
            "nested/a.rs:2\n",
        ),
        (
            "Grep",
            json!({"pattern":"βeta","glob":"**/*.rs","output_mode":"content","context":1,"head_limit":1}),
            "nested/a.rs-1-alpha\nnested/a.rs:2:βeta\nnested/a.rs-3-βeta\n",
        ),
        (
            "Grep",
            json!({"pattern":"βeta\nβeta","multiline":true,"output_mode":"content"}),
            "nested/a.rs:2:βeta\nnested/a.rs:3:βeta\n",
        ),
        (
            "Grep",
            json!({"pattern":"β.ta","-o":true,"output_mode":"content","offset":1}),
            "nested/a.rs:3:βeta\n",
        ),
    ] {
        let portable = plan(name, input.clone(), files.clone()).unwrap();
        let output = native
            .execute_output_with_context(name, input.clone(), false)
            .await
            .unwrap();
        let nanocodex_claude_tools::ToolContent::Text(actual) = output.content else {
            panic!("expected text")
        };
        assert_eq!(portable.output, expected);
        assert_eq!(actual, expected);
        println!("{name} {input} => {actual:?} (native == portable)");
    }
    let prepare = json!({"root":"/workspace","name":"Grep","input":{"pattern":"β","type":"rust"},"prepare":true,"files":[{"path":"nested/a.rs","size":100},{"path":"ignored.bin","size":999999999}]});
    let prepared = portable_plan::plan(serde_json::from_value(prepare).unwrap()).unwrap();
    assert_eq!(prepared.reads, vec!["nested/a.rs"]);
    assert!(prepared.mutations.is_empty());
    for (name, input, files) in [
        ("Grep", json!({"pattern":"(?=unsupported)"}), files.clone()),
        (
            "Grep",
            json!({"pattern":"x","type":"not-a-type"}),
            files.clone(),
        ),
        (
            "Write",
            json!({"file_path":"a","content":"x".repeat(1024*1024+1)}),
            json!([]),
        ),
        (
            "Read",
            json!({"file_path":"image.png"}),
            json!([{"path":"image.png","content":"bad"}]),
        ),
    ] {
        assert!(plan(name, input, files).is_err());
        println!("{name}: bounded/invalid/capability error verified");
    }
    let note = json!({"nbformat":4,"nbformat_minor":5,"metadata":{},"cells":[{"id":"a","cell_type":"code","metadata":{"keep":true},"source":["x=1\n"],"execution_count":null,"outputs":[]}]});
    fs::write(dir.path().join("note.ipynb"), note.to_string()).unwrap();
    let notebook = ClaudeNotebook::new(dir.path()).unwrap();
    let mut before = note.to_string();
    for input in [
        json!({"notebook_path":"note.ipynb","cell_id":"a","new_source":"# hello","cell_type":"markdown"}),
        json!({"notebook_path":"note.ipynb","new_source":"x=2","cell_type":"code","edit_mode":"insert"}),
        json!({"notebook_path":"note.ipynb","new_source":"","cell_id":"a","edit_mode":"delete"}),
    ] {
        let result = plan(
            "NotebookEdit",
            input.clone(),
            json!([{"path":"note.ipynb","content":before}]),
        )
        .unwrap();
        notebook
            .execute("NotebookEdit", input.clone())
            .await
            .unwrap();
        before = fs::read_to_string(dir.path().join("note.ipynb")).unwrap();
        assert_eq!(result.mutations[0].content, before);
        let notebook: Value = serde_json::from_str(&before).unwrap();
        if input.get("edit_mode").is_none() {
            assert_eq!(notebook["cells"][0]["metadata"]["keep"], true);
            assert_eq!(notebook["cells"][0]["cell_type"], "markdown");
            assert!(notebook["cells"][0].get("outputs").is_none());
            assert_eq!(notebook["cells"][0]["source"], json!(["# hello"]));
        }

        println!("NotebookEdit {input}: native == portable; metadata retained");
    }
    let result = plan(
        "Read",
        json!({"file_path":"note.ipynb"}),
        json!([{"path":"note.ipynb","content":before}]),
    )
    .unwrap();
    assert!(result.output.contains("x=2"));
    println!("Notebook Read => {:?}", result.output);
}
