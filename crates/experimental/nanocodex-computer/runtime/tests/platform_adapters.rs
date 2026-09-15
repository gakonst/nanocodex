use serde_json::{Value, json};
use skyre::platforms::{Platforms, linux::Settler, windows::lower};
use std::{
    fs,
    time::{Duration, Instant},
};
fn python() -> String {
    for p in ["/usr/bin/python3", "/opt/homebrew/bin/python3"] {
        if std::path::Path::new(p).exists() {
            return p.into();
        }
    }
    panic!("Python fixture interpreter unavailable")
}
fn script(root: &std::path::Path, name: &str, text: &str) -> String {
    let path = root.join(name);
    fs::write(&path, text).unwrap();
    path.to_string_lossy().to_string()
}
fn run(p: &mut Platforms, id: &str, method: &str, args: Value, turn: &str) -> skyre::Result<Value> {
    p.set_turn_context("fixture", turn)?;
    p.execute(
        "platform.call",
        &json!({"id":id,"method":method,"params":args}),
    )
}
#[test]
fn platform_windows_lowering_prioritizes_elements_and_validates_boundaries() {
    let w = json!({"app":"fixture.exe","id":"0"});
    let(method,args)=lower("click",&json!({"window":w,"element":3,"elementIndex":4,"element_index":"5","x":1,"y":2,"screenshotId":"old"})).unwrap();
    assert_eq!(method, "click_element");
    assert_eq!(args["element_index"], 5);
    assert!(args.get("screenshotId").is_none());
    let (_, args) = lower("click", &json!({"window":w,"x":-1.5,"y":2.5})).unwrap();
    assert_eq!(args["x"], -1.0);
    assert_eq!(args["y"], 3.0);
    assert!(
        lower(
            "get_window_state",
            &json!({"window":w,"include_screenshot":false,"include_text":false})
        )
        .is_err()
    );
    assert!(lower("drag", &json!({"window":w,"from_x":1})).is_err());
    assert!(lower("click", &json!({"window":w,"element_index":-1})).is_err());
    assert!(lower("start_audio_recording", &json!({"max_duration_ms":99})).is_err());
    let (_, args) = lower("press_key", &json!({"window":w,"key":" CTRL + + A "})).unwrap();
    assert_eq!(args["key"], "CTRL+A");
}
#[test]
fn platform_linux_process_lowering_and_drag_lifecycle() {
    let temp = tempfile::tempdir().unwrap();
    let helper = script(
        temp.path(),
        "linux.py",
        "import sys,json\np=json.load(sys.stdin)\nif p.get('fail'):sys.exit(9)\nprint(json.dumps({'argv':sys.argv[1:],'params':p}))\n",
    );
    let mut p = Platforms::new();
    p.configure(&json!({"id":"linux","kind":"linux_helper","executable":python(),"args":[helper],"mouse_size_px":22,"post_action_sleep_ms":0})).unwrap();
    let result = run(&mut p, "linux", "click", json!({"x":10,"y":20}), "a").unwrap();
    assert_eq!(result["argv"], json!(["--mouse-size-px", "22", "click"]));
    let handle =
        run(&mut p, "linux", "drag_handle.create", json!({}), "a").unwrap()["handle"].clone();
    assert!(
        run(
            &mut p,
            "linux",
            "drag_handle.move_to",
            json!({"handle":handle,"x":1,"y":2}),
            "a"
        )
        .is_err()
    );
    assert!(
        run(
            &mut p,
            "linux",
            "drag_handle.start",
            json!({"handle":handle,"fail":true}),
            "a"
        )
        .is_err()
    );
    let start = run(
        &mut p,
        "linux",
        "drag_handle.start",
        json!({"handle":handle,"x":1,"y":2}),
        "a",
    )
    .unwrap();
    assert_eq!(start["params"]["action"], "start");
    assert!(start["params"].get("handle").is_none());
    assert!(
        run(
            &mut p,
            "linux",
            "drag_handle.start",
            json!({"handle":handle}),
            "a"
        )
        .is_err()
    );
    run(
        &mut p,
        "linux",
        "drag_handle.move_to",
        json!({"handle":handle,"x":3,"y":4}),
        "a",
    )
    .unwrap();
    run(
        &mut p,
        "linux",
        "drag_handle.end",
        json!({"handle":handle}),
        "a",
    )
    .unwrap();
    assert!(
        run(
            &mut p,
            "linux",
            "drag_handle.end",
            json!({"handle":handle}),
            "a"
        )
        .is_err()
    );
    p.end_turn().unwrap();
    assert!(
        run(
            &mut p,
            "linux",
            "drag_handle.start",
            json!({"handle":handle}),
            "a"
        )
        .is_err()
    );
}
#[test]
fn platform_windows_persistent_turns_events_approval_and_recoverable_error() {
    let temp = tempfile::tempdir().unwrap();
    let helper = script(
        temp.path(),
        "windows.py",
        r#"import sys,json
history=[]
for line in sys.stdin:
 r=json.loads(line);history.append(r['method']);method=r['method']
 if method=='type_text' and r['params']['text']=='approval': print(json.dumps({'id':r['id'],'ok':False,'approvalRequest':{'app':'computer-audio','displayName':'Audio'}}),flush=True);continue
 if method=='type_text' and r['params']['text']=='error': print(json.dumps({'id':r['id'],'ok':False,'error':'fixture operation failure'}),flush=True);continue
 print(json.dumps({'event':'observed','method':method}),flush=True)
 print(json.dumps({'id':r['id'],'ok':True,'result':{'history':list(history),'request':r}}),flush=True)
"#,
    );
    let mut p = Platforms::new();
    p.configure(
        &json!({"id":"windows","kind":"windows_helper","executable":python(),"args":[helper]}),
    )
    .unwrap();
    let a = run(&mut p, "windows", "list_apps", json!({}), "a").unwrap();
    assert_eq!(a["history"], json!(["list_apps"]));
    assert_eq!(a["request"]["meta"]["x-oai-cua-request-budget-ms"], 15000);
    let b = run(&mut p, "windows", "list_apps", json!({}), "b").unwrap();
    assert_eq!(b["history"], json!(["list_apps", "end_turn", "list_apps"]));
    let w = json!({"app":"fixture","id":1});
    let approval = run(
        &mut p,
        "windows",
        "type_text",
        json!({"window":w,"text":"approval"}),
        "b",
    )
    .unwrap();
    assert_eq!(
        approval["approval_required"]["allowPersistentApproval"],
        false
    );
    assert_eq!(
        run(
            &mut p,
            "windows",
            "type_text",
            json!({"window":w,"text":"error"}),
            "b"
        )
        .unwrap_err()
        .code,
        -10005
    );
    let after = run(&mut p, "windows", "list_apps", json!({}), "b").unwrap();
    assert_eq!(after["history"].as_array().unwrap().len(), 6);
    assert_eq!(
        p.execute("platform.events", &json!({"id":"windows"}))
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        4
    );
    assert_eq!(
        p.execute("platform.events", &json!({"id":"windows"}))
            .unwrap(),
        json!([])
    );
    p.execute("platform.unregister", &json!({"id":"windows"}))
        .unwrap();
}
#[test]
fn platform_windows_escape_persists_per_turn() {
    let temp = tempfile::tempdir().unwrap();
    let helper = script(
        temp.path(),
        "escape.py",
        r#"import sys,json,time
for line in sys.stdin:
 r=json.loads(line)
 if r['params'].get('text')=='escape': print(json.dumps({'id':r['id'],'ok':False,'error':'Computer Use was stopped by the user with the physical Escape key'}),flush=True);continue
 if r['params'].get('text')=='slow':time.sleep(5)
 print(json.dumps({'id':r['id'],'ok':True,'result':{'alive':True}}),flush=True)
"#,
    );
    let config = json!({"id":"w","kind":"windows_helper","executable":python(),"args":[helper],"timeout_ms":10000,"state_directory":temp.path().join("interrupts")});
    let mut p = Platforms::new();
    p.configure(&config).unwrap();
    let w = json!({"app":"fixture","id":1});
    assert_eq!(
        run(
            &mut p,
            "w",
            "type_text",
            json!({"window":w,"text":"escape"}),
            "a"
        )
        .unwrap_err()
        .code,
        -32010
    );
    assert_eq!(
        run(&mut p, "w", "list_apps", json!({}), "a")
            .unwrap_err()
            .code,
        -32010
    );
    drop(p);
    let mut p = Platforms::new();
    p.configure(&config).unwrap();
    assert_eq!(
        run(&mut p, "w", "list_apps", json!({}), "a")
            .unwrap_err()
            .code,
        -32010
    );
    assert_eq!(
        run(&mut p, "w", "list_apps", json!({}), "b").unwrap()["alive"],
        true
    );
}
#[test]
fn platform_process_deadline_kills_pipe_holding_descendant() {
    let temp = tempfile::tempdir().unwrap();
    let helper = script(
        temp.path(),
        "descendant.py",
        "import subprocess,time\nsubprocess.Popen(['sleep','30'])\ntime.sleep(30)\n",
    );
    let began = Instant::now();
    let error = skyre::platforms::process::run(
        std::path::Path::new(&python()),
        &[helper],
        &[],
        Duration::from_millis(100),
    )
    .unwrap_err();
    assert_eq!(error.code, -32008);
    assert!(began.elapsed() < Duration::from_secs(2));
}
#[test]
fn platform_settler_extends_monotonic_deadline() {
    let mut settler = Settler::new(Duration::from_millis(20));
    assert!(settler.remaining().is_zero());
    settler.defer();
    std::thread::sleep(Duration::from_millis(10));
    settler.defer();
    assert!(settler.remaining() >= Duration::from_millis(15));
    settler.wait();
    assert!(settler.remaining().is_zero());
}

#[test]
fn platform_x11_uses_argument_vectors_and_stdin_without_shell_injection() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let helper = temp.path().join("xdotool");
        let log = temp.path().join("calls.jsonl");
        let script = format!(
            "#!/usr/bin/python3\nimport json,sys\nwith open({:?},'a') as f:f.write(json.dumps({{'args':sys.argv[1:],'input':sys.stdin.read()}})+'\\n')\n",
            log.to_str().unwrap()
        );
        fs::write(&helper, script).unwrap();
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
        let mut platforms = Platforms::new();
        platforms
            .configure(
                &json!({"id":"x11","kind":"linux_x11","xdotool":helper,"screenshot_tool":helper}),
            )
            .unwrap();
        run(
            &mut platforms,
            "x11",
            "type_text",
            json!({"text":"$(touch /tmp/should-not-exist); quotes ' \""}),
            "x",
        )
        .unwrap();
        run(
            &mut platforms,
            "x11",
            "click",
            json!({"x":10.3,"y":20.8,"mouse_button":"right","click_count":2}),
            "x",
        )
        .unwrap();
        let calls: Vec<Value> = fs::read_to_string(log)
            .unwrap()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();
        assert_eq!(
            calls[0]["args"],
            json!(["type", "--clearmodifiers", "--file", "-"])
        );
        assert!(calls[0]["input"].as_str().unwrap().starts_with("$(touch"));
        assert_eq!(
            calls[1]["args"],
            json!([
                "mousemove",
                "--sync",
                "10",
                "21",
                "click",
                "--repeat",
                "2",
                "--delay",
                "100",
                "3"
            ])
        );
    }
}

#[test]
fn platform_persistent_stdin_backpressure_obeys_deadline() {
    let temp = tempfile::tempdir().unwrap();
    let helper = script(
        temp.path(),
        "never_read.py",
        "import time\ntime.sleep(30)\n",
    );
    let mut platforms = Platforms::new();
    platforms.configure(&json!({"id":"blocked","kind":"windows_helper","executable":python(),"args":[helper],"timeout_ms":200})).unwrap();
    let started = Instant::now();
    let error = run(
        &mut platforms,
        "blocked",
        "type_text",
        json!({"window":{"app":"fixture","id":1},"text":"x".repeat(1024*1024)}),
        "turn",
    )
    .unwrap_err();
    assert_eq!(error.code, -32008);
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn platform_rpc_cannot_forge_turn_or_approval_metadata() {
    let mut platforms = Platforms::new();
    platforms
        .set_turn_context("actual-session", "actual-turn")
        .unwrap();
    assert_eq!(platforms.execute("platform.call",&json!({"id":"unknown","method":"list_apps","params":{},"meta":{"session_id":"forged","turn_id":"forged","approved_app":"any"}})).unwrap_err().code,-32003);
}
