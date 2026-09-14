use serde_json::{Value, json};
use skyre::{
    Error, Result,
    ax::{Node, Revision, Sessions, kept_positions, ranges},
    clipboard::{Item, Pasteboard, Transaction},
    engine::{Engine, decode_native_action},
    fixture::Fixture,
    protocol,
    runtime::Host,
    selection::{Mode, TextRange, replace_utf16, select},
};
use std::{cell::RefCell, collections::BTreeSet, rc::Rc, time::Duration};
fn leaf(identity: &str) -> Node {
    Node {
        identity: identity.into(),
        role: "AXButton".into(),
        title: Some(identity.into()),
        enabled: true,
        ..Default::default()
    }
}
fn tree(ids: &[&str]) -> Node {
    Node {
        identity: "root".into(),
        role: "AXWindow".into(),
        enabled: true,
        children: ids.iter().map(|s| leaf(s)).collect(),
        ..Default::default()
    }
}

#[test]
fn utf16_selection_observed_modes_and_adjacent_context() {
    let s = "red alpha blue alpha green";
    assert!(select(s, "alpha", None, None, Mode::Text).is_err());
    for (mode, location, length) in [
        (Mode::Text, 15, 5),
        (Mode::CursorBefore, 15, 0),
        (Mode::CursorAfter, 20, 0),
    ] {
        assert_eq!(
            select(s, "alpha", Some("blue "), Some(" green"), mode).unwrap(),
            TextRange { location, length }
        );
    }
    assert!(select(s, "alpha", Some("red"), None, Mode::Text).is_err());
    assert!(select("aaa", "aa", None, None, Mode::Text).is_err()); // overlap counts
    assert!(select(s, "", None, None, Mode::Text).is_err());
    assert_eq!(
        select("A🧪β終Z", "β終", None, None, Mode::Text).unwrap(),
        TextRange {
            location: 3,
            length: 2
        }
    );
    assert_eq!(
        replace_utf16(
            "A🧪Z",
            TextRange {
                location: 1,
                length: 2
            },
            "β終"
        )
        .unwrap(),
        "Aβ終Z"
    );
    assert!(
        replace_utf16(
            "🧪",
            TextRange {
                location: 1,
                length: 0
            },
            "x"
        )
        .is_err()
    );
    assert!(
        replace_utf16(
            "a",
            TextRange {
                location: usize::MAX,
                length: 2
            },
            "x"
        )
        .is_err()
    );
}

#[test]
fn lis_matches_exhaustive_earliest_subsequence_oracle() {
    // Exhaust all words over 0..4 up to length 7, including duplicate positions.
    for len in 0..=7 {
        for mut bits in 0..4_usize.pow(len) {
            let mut word = vec![0; len as usize];
            for n in &mut word {
                *n = bits % 4;
                bits /= 4;
            }
            let mut best = vec![];
            for mask in 0..(1 << len) {
                let candidate: Vec<_> =
                    (0..len as usize).filter(|i| mask & (1 << i) != 0).collect();
                if candidate.windows(2).all(|p| word[p[0]] < word[p[1]])
                    && (candidate.len() > best.len()
                        || (candidate.len() == best.len() && candidate < best))
                {
                    best = candidate;
                }
            }
            assert_eq!(
                kept_positions(&word).into_iter().collect::<Vec<_>>(),
                best,
                "{word:?}"
            );
        }
    }
}

#[test]
fn revisions_reorder_reallocate_and_budget() {
    let old = Revision::root(tree(&["a", "b", "c"]));
    assert_eq!(
        old.root
            .children
            .iter()
            .map(|n| n.id.unwrap())
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    let moved = old.append(tree(&["b", "a", "c"])).unwrap();
    assert_eq!(
        moved
            .root
            .children
            .iter()
            .map(|n| n.id.unwrap())
            .collect::<Vec<_>>(),
        vec![2, 4, 3]
    );
    assert!(moved.diff(&old).unwrap().contains("Removed element IDs: 1"));
    let root = Node {
        identity: "other".into(),
        enabled: true,
        ..Default::default()
    };
    let new = old.append(root).unwrap();
    assert_eq!(new.root.id, Some(1));
    assert!(new.diff(&old).is_none());
    assert_eq!(
        old.append(tree(&["a", "b", "c"])).unwrap().diff(&old),
        Some(String::new())
    );
    assert_eq!(
        ranges(&BTreeSet::from([0, 1, 2, 4, u64::MAX])),
        format!("0-2, 4, {}", u64::MAX)
    );
}

#[test]
fn refetch_failure_order_and_published_revision() {
    let root = tree(&["a"]);
    let mut sessions = Sessions::default();
    sessions.observe("a", root.clone(), false).unwrap();
    let mut replacement = root.clone();
    replacement.children[0].identity = "replacement".into();
    assert_eq!(
        sessions
            .resolve("a", 1, replacement.clone(), false)
            .unwrap()
            .identity,
        "replacement"
    );
    assert_eq!(sessions.revisions["a"].generation, 1);
    // Missing ID fails before any publication.
    assert!(sessions.resolve("a", 999, replacement, false).is_err());
    assert_eq!(sessions.revisions["a"].generation, 1);
    sessions.observe("a", root.clone(), true).unwrap();
    let mut duplicate = root.clone();
    duplicate.children[0].identity = "new-a".into();
    let mut second = duplicate.children[0].clone();
    second.identity = "new-b".into();
    duplicate.children.push(second);
    assert!(
        sessions
            .resolve("a", 1, duplicate.clone(), false)
            .unwrap_err()
            .message
            .contains("after refetch")
    );
    assert_eq!(sessions.revisions["a"].generation, 1);
    let numbered_id = sessions.revisions["a"].root.children[0].id.unwrap();
    let mut changed = duplicate.clone();
    changed.children[0].value = Some("changed".into());
    assert!(
        sessions
            .resolve("a", numbered_id, changed, false)
            .unwrap_err()
            .message
            .contains("before refetch")
    );
    assert_eq!(sessions.revisions["a"].generation, 1);
    // Unchanged concrete handle wins even if semantic duplicates exist.
    assert!(sessions.resolve("a", numbered_id, duplicate, false).is_ok());
}

#[test]
fn refetch_ignores_only_value_when_requested() {
    let mut root = tree(&["a"]);
    root.children[0].value = Some("old".into());
    let mut sessions = Sessions::default();
    sessions.observe("a", root.clone(), false).unwrap();
    root.children[0].value = Some("new".into());
    root.children[0].focused = true;
    root.children[0].enabled = false;
    root.children[0].frame = Some([1., 2., 3., 4.]);
    assert!(sessions.resolve("a", 1, root.clone(), true).is_ok());
    root.children[0].title = Some("different".into());
    assert!(sessions.resolve("a", 1, root, true).is_err());
}

#[test]
fn framing_every_split_and_coalesced_unicode() {
    let values = [
        json!({"jsonrpc":"2.0","id":1,"result":"β🧪"}),
        json!({"jsonrpc":"2.0","id":"2","error":{"code":-1,"message":"failure"}}),
    ];
    let bytes: Vec<_> = values
        .iter()
        .flat_map(|v| protocol::encode(v).unwrap())
        .collect();
    for split in 0..=bytes.len() {
        let mut decoder = protocol::Decoder::default();
        let mut out = decoder.feed(&bytes[..split]).unwrap();
        out.extend(decoder.feed(&bytes[split..]).unwrap());
        assert_eq!(out, values);
        decoder.finish().unwrap();
    }
    let mut decoder = protocol::Decoder::default();
    for b in &bytes {
        decoder.feed(&[*b]).unwrap();
    }
    decoder.finish().unwrap();
    let mut decoder = protocol::Decoder::default();
    decoder.feed(&bytes[..3]).unwrap();
    assert!(decoder.finish().is_err());
    let mut input = &bytes[..];
    for v in values {
        assert_eq!(protocol::read_frame(&mut input).unwrap(), Some(v));
    }
    assert_eq!(protocol::read_frame(&mut input).unwrap(), None);
    assert!(protocol::read_frame(&mut &bytes[..bytes.len() - 1]).is_ok());
    assert!(protocol::read_frame(&mut &bytes[..2]).is_err());
}

#[test]
fn jsonrpc_envelopes_and_malformed_responses() {
    for invalid in [
        json!({}),
        json!({"jsonrpc":"1","method":"ping"}),
        json!({"jsonrpc":"2.0","method":"ping","id":[]}),
        json!({"jsonrpc":"2.0","method":"ping","params":null}),
    ] {
        assert!(protocol::validate_request(&invalid).is_err());
    }
    for invalid in [
        json!({"jsonrpc":"2.0","id":1}),
        json!({"jsonrpc":"2.0","id":1,"result":null,"error":{}}),
        json!({"jsonrpc":"2.0","id":1,"error":{"code":"1","message":"bad"}}),
    ] {
        assert!(protocol::validate_response(&invalid).is_err());
    }
    protocol::validate_response(&protocol::response(json!("id"), Err(Error::action("bad"))))
        .unwrap();
}

#[test]
fn native_action_contract_variants() {
    for (action, method, expected) in [
        (
            json!({"click":{"at":{"elementID":{"_0":"7"}},"clickCount":2,"mouseButton":1}}),
            "click",
            json!({"element_index":7}),
        ),
        (
            json!({"click":{"at":{"coordinate":{"_0":[2,3]}},"clickCount":1}}),
            "click",
            json!({"point":[2,3]}),
        ),
        (
            json!({"drag":{"from":[1,2],"to":[3,4]}}),
            "drag",
            json!({"to":[3,4]}),
        ),
        (
            json!({"paste":{"text":"x","format":"md"}}),
            "paste",
            json!({"format":"md"}),
        ),
        (
            json!({"performSecondaryAction":{"elementID":"5","action":"AXShowMenu"}}),
            "perform_secondary_action",
            json!({"element_index":5}),
        ),
        (
            json!({"pressKey":{"_0":"CMD+C"}}),
            "press_key",
            json!({"key":"CMD+C"}),
        ),
        (
            json!({"scroll":{"at":{"coordinate":{"_0":[1,2]}},"direction":"down","pages":2}}),
            "scroll",
            json!({"pages":2}),
        ),
        (
            json!({"setValue":{"elementID":"4","value":"x"}}),
            "set_value",
            json!({"value":"x"}),
        ),
        (
            json!({"selectText":{"elementID":"4","text":"x","selection":"cursor_after"}}),
            "select_text",
            json!({"selection":"cursor_after"}),
        ),
        (
            json!({"type":{"_0":"🧪"}}),
            "type_text",
            json!({"text":"🧪"}),
        ),
    ] {
        let (m, args) =
            decode_native_action(&json!({"app":"fixture://native","action":action})).unwrap();
        assert_eq!(m, method);
        assert_eq!(args["app"], "fixture://native");
        for (k, v) in expected.as_object().unwrap() {
            assert_eq!(&args[k], v);
        }
    }
    assert!(decode_native_action(&json!({"action":{"click":{},"type":{}}})).is_err());
    assert!(decode_native_action(&json!({"action":{"setValue":{"elementID":"-1"}}})).is_err());
}

#[derive(Default)]
struct Board {
    generation: i64,
    items: Vec<Item>,
    consumed: bool,
    event: Option<&'static str>,
    writes: usize,
    fail_read: bool,
    fail_write: Option<usize>,
}
impl Pasteboard for Board {
    fn generation(&self) -> i64 {
        self.generation
    }
    fn snapshot(&self) -> Result<Vec<Item>> {
        if self.fail_read {
            Err(Error::action("unreadable"))
        } else {
            Ok(self.items.clone())
        }
    }
    fn install(&mut self, items: &[Item], expected: i64) -> Result<Option<i64>> {
        if self.generation != expected {
            return Ok(None);
        }
        self.writes += 1;
        if self.fail_write == Some(self.writes) {
            return Err(Error::action("write failed"));
        }
        self.items = items.to_vec();
        self.generation += 1;
        Ok(Some(self.generation))
    }
    fn consumed(&self) -> bool {
        self.consumed
    }
    fn poll(&mut self, _: Duration) {
        match self.event.take() {
            Some("consume") => self.consumed = true,
            Some("writer") => {
                self.items = vec![item("competitor")];
                self.generation += 1;
            }
            _ => {}
        }
    }
}
fn item(s: &str) -> Item {
    Item::from([
        ("public.utf8-plain-text".into(), s.as_bytes().to_vec()),
        ("org.skyre.fixture".into(), vec![0, 255, 10]),
    ])
}
#[test]
fn clipboard_restore_ownership_timeout_drop_and_failures() {
    for event in [Some("consume"), Some("writer"), None] {
        let saved = vec![item("old"), Item::new()];
        let mut board = Board {
            items: saved.clone(),
            event,
            ..Default::default()
        };
        let mut tx = Transaction::begin(&mut board, &[item("new")]).unwrap();
        let r = tx.wait(Duration::from_millis(2));
        assert_eq!(r.is_ok(), event == Some("consume"));
        let restored = tx.finish().unwrap();
        assert_eq!(restored, event != Some("writer"));
        assert_eq!(
            board.items,
            if restored {
                saved
            } else {
                vec![item("competitor")]
            }
        );
    }
    let mut board = Board {
        items: vec![item("old")],
        ..Default::default()
    };
    drop(Transaction::begin(&mut board, &[item("new")]).unwrap());
    assert_eq!(board.items, vec![item("old")]);
    let mut board = Board {
        items: vec![item("old")],
        fail_read: true,
        ..Default::default()
    };
    assert!(Transaction::begin(&mut board, &[item("new")]).is_err());
    assert_eq!(board.writes, 0);
    let mut board = Board {
        items: vec![item("old")],
        fail_write: Some(2),
        ..Default::default()
    };
    assert!(
        Transaction::begin(&mut board, &[item("new")])
            .unwrap()
            .finish()
            .is_err()
    );
    assert_eq!(board.writes, 2); // no destructive Drop retry
}

fn host() -> (Rc<RefCell<Engine>>, Host) {
    let engine = Rc::new(RefCell::new(Engine::new(Box::new(Fixture::default()))));
    let host = Host::new(engine.clone()).unwrap();
    (engine, host)
}
fn eval(host: &mut Host, code: &str) -> Value {
    let value = host.evaluate(code, Duration::from_secs(3)).unwrap();
    assert!(value.get("error").is_none(), "{value}");
    value
}
#[test]
fn persistent_runtime_tla_docs_errors_reset_and_external_state() {
    let (engine, mut h) = host();
    let r = eval(&mut h, "var app = await cua.getApp('fixture://native');");
    assert_eq!(
        r["outputs"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|v| v["channel"] == "cua.core")
            .count(),
        1
    );
    let r = eval(
        &mut h,
        "await app.selectText(1,'alpha',{prefix:'blue ',suffix:' green'}); await app.typeText('🧪'); await app.getAXState();",
    );
    assert_eq!(
        r["outputs"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|v| v["channel"] == "cua.core")
            .count(),
        0
    );
    assert!(r.to_string().contains("red alpha blue 🧪 green"));
    assert!(
        h.evaluate("await app.selectText(1,'missing');", Duration::from_secs(1))
            .unwrap()
            .get("error")
            .is_some()
    );
    assert!(
        h.evaluate("while(true){}", Duration::from_millis(10))
            .unwrap()
            .get("error")
            .is_some()
    );
    eval(&mut h, "nodeRepl.write('recovered');");
    drop(h);
    let mut h = Host::new(engine).unwrap();
    assert!(
        h.evaluate("await app.getAXState()", Duration::from_secs(1))
            .unwrap()
            .get("error")
            .is_some()
    );
    let r = eval(&mut h, "var app = await cua.getApp('fixture://native');");
    assert!(r.to_string().contains("red alpha blue 🧪 green"));
}

#[test]
fn engine_rejects_invalid_action_without_mutating_fixture() {
    let mut e = Engine::new(Box::new(Fixture::default()));
    e.execute("get_app_state", &json!({"app":"fixture://native"}))
        .unwrap();
    for args in [
        json!({"clickCount":0}),
        json!({"clickCount":4}),
        json!({"mouseButton":"bogus"}),
    ] {
        let mut a = args;
        a["app"] = json!("fixture://native");
        a["element_index"] = json!(2);
        assert!(e.execute("click", &a).is_err());
    }
    assert_eq!(
        e.execute("fixture.state", &json!({})).unwrap()["actions"],
        json!([])
    );
    assert!(e.native_request(&json!({"clientApiVersion":protocol::IPC_VERSION,"deadlineUnixMilliseconds":1,"requestType":"ComputerUseIPCListAppsRequest","request":{}})).is_err());
}

#[test]
fn rich_text_is_resource_free_and_preserves_unicode_and_styles() {
    use skyre::rich_text::representations;
    let payload=representations("<p>Hello <b>β🧪</b> &amp; {x}</p><img sRc = 'https://example.invalid/a' alt='ALT'><style>@import 'x'</style><script>secret</script><a href='javascript:evil()'>link</a>","html").unwrap();
    let plain = String::from_utf8(payload["public.utf8-plain-text"].clone()).unwrap();
    assert_eq!(plain, "Hello β🧪 & {x}\nALTlink");
    let html = String::from_utf8(payload["public.html"].clone()).unwrap();
    assert!(html.contains("<b>β🧪</b>"));
    assert!(!html.contains("https:"));
    assert!(!html.contains("javascript:"));
    assert!(!html.contains("secret"));
    let rtf = String::from_utf8(payload["public.rtf"].clone()).unwrap();
    assert!(rtf.contains("\\b "));
    assert!(rtf.contains("\\u946?"));
    assert!(rtf.contains("\\u-10178?\\u-8726?"));
    assert!(rtf.contains("\\{x\\}"));
    assert!(
        String::from_utf8(
            representations("**bold** and *italic*", "md").unwrap()["public.html"].clone()
        )
        .unwrap()
        .contains("<strong>bold</strong>")
    );
    assert!(representations("x", "unknown").is_err());
    assert!(representations(&"a".repeat(1024 * 1024 + 1), "html").is_err());
}
#[test]
fn runtime_images_are_bytes_and_output_count_is_not_artificially_bounded() {
    let (_, mut h) = host();
    eval(&mut h, "let app = await cua.getApp('fixture://native');");
    let r = eval(
        &mut h,
        "let shot = await app.getScreenshot({emit:false});nodeRepl.write([shot instanceof Uint8Array,shot[0],shot[1]]);await nodeRepl.emitImage(shot);",
    );
    assert_eq!(r["outputs"][0]["value"], "[ true, 137, 80 ]");
    assert_eq!(r["outputs"][1]["channel"], "image");
    assert_eq!(r["outputs"][1]["value"]["mime_type"], "image/png");
    let many_outputs = h
        .evaluate(
            "for(let i=0;i<300;i++)nodeRepl.write({i})",
            Duration::from_secs(1),
        )
        .unwrap();
    assert!(many_outputs.get("error").is_none(), "{many_outputs}");
    assert_eq!(
        eval(&mut h, "nodeRepl.write('next cell')")["outputs"][0]["value"],
        "next cell"
    );
}

struct MutatingSetter {
    fixture: Fixture,
}
impl skyre::native::Desktop for MutatingSetter {
    fn apps(&mut self) -> Result<Vec<skyre::native::App>> {
        self.fixture.apps()
    }
    fn snapshot(&mut self, a: &skyre::native::App) -> Result<Node> {
        self.fixture.snapshot(a)
    }
    fn action(&mut self, a: &skyre::native::App, action: skyre::native::Action) -> Result<()> {
        self.fixture.action(a, action)?;
        self.fixture.root.children[0].title = Some("changed during setter".into());
        Ok(())
    }
    fn capabilities(&self) -> Vec<&'static str> {
        vec![]
    }
    fn control_fixture(&mut self, m: &str, a: &Value) -> Result<Value> {
        self.fixture.control_fixture(m, a)
    }
}
#[test]
fn setter_can_mutate_before_post_validation_failure() {
    let mut e = Engine::new(Box::new(MutatingSetter {
        fixture: Fixture::default(),
    }));
    e.execute("get_app_state", &json!({"app":"fixture://native"}))
        .unwrap();
    let error = e
        .execute(
            "set_value",
            &json!({"app":"fixture://native","element_index":1,"value":"write survived"}),
        )
        .unwrap_err();
    assert!(error.message.contains("after refetch"));
    assert_eq!(
        e.execute("fixture.state", &json!({})).unwrap()["root"]["children"][0]["value"],
        "write survived"
    );
    assert_eq!(e.sessions.revisions["fixture://native"].generation, 1);
}

#[test]
fn native_points_are_window_relative_on_negative_origin_displays() {
    use skyre::native::window_point;
    assert_eq!(
        window_point([30., 558., 1120., 852.], [100., 200.]).unwrap(),
        [130., 758.]
    );
    assert_eq!(
        window_point([-1920., -1080., 800., 600.], [10., 20.]).unwrap(),
        [-1910., -1060.]
    );
    for point in [[-1., 0.], [800., 0.], [0., 600.], [f64::NAN, 0.]] {
        assert!(window_point([0., 0., 800., 600.], point).is_err());
    }
}
