//! Native formatter failure shape and private engine-cache lifetime controls.
//! The unchanged runtime_app_state_instructions target owns the four original
//! criteria through actual Host and Worker paths; these are additional controls.
use serde_json::{Value, json};
use skyre::{
    Error, Result,
    runtime::{Host, HostOptions, RuntimeBackend},
};
use std::{
    cell::RefCell,
    collections::VecDeque,
    rc::Rc,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

const TREE: &str = "owned tree";
const PREFIX: &str =
    "<app_specific_instructions>\nowned guidance\n</app_specific_instructions>\nowned tree";
fn dto() -> Value {
    json!({"app":{"bundleIdentifier":"owned.native.formatter"},"skyshot":{"text":TREE},"appSpecificInstructions":"owned guidance"})
}
struct Owner {
    host: Host,
    replies: Rc<RefCell<VecDeque<Result<Value>>>>,
    calls: Rc<RefCell<Vec<String>>>,
}
impl Owner {
    fn new(runtime: RuntimeBackend) -> Self {
        let replies = Rc::new(RefCell::new(VecDeque::new()));
        let queue = replies.clone();
        let calls = Rc::new(RefCell::new(Vec::new()));
        let trace = calls.clone();
        let host = Host::with_controlled_dispatch(
            move |method, args, control| {
                control.validate().expect("live owned provider call, including suspended calls");
                trace.borrow_mut().push(method.to_owned());
                match method {
                    "sky.setup" => Ok(json!({"target":"mac","methods":["get_app_state"]})),
                    "sky.app_policy" => {
                        assert_eq!(*args, json!({"app":"Owned"}));
                        Ok(json!({"decision":"allowed","allowPersistentApproval":false,
                            "target":{"bundleIdentifier":"owned.native.formatter","displayName":"Owned native formatter","appPath":"/owned/Native.app","risk":"low"}}))
                    }
                    "host.elicitation" => Ok(json!({"action":"accept"})),
                    "sky.execute" => {
                        assert_eq!(*args, json!({"method":"get_app_state","args":[{"app":"/owned/Native.app"}]}));
                        queue.borrow_mut().pop_front().expect("an expected owned DTO reply")
                    }
                    _ => panic!("Unexpected native formatter provider method: {method}"),
                }
            },
            Arc::new(AtomicBool::new(false)),
            HostOptions { runtime, request_meta: Some(json!({"x-codex-turn-metadata":{"call_id":"native-formatter"}})), ..Default::default() },
        ).unwrap();
        Self {
            host,
            replies,
            calls,
        }
    }
    fn request(&mut self, reply: Result<Value>) -> Value {
        self.replies.borrow_mut().push_back(reply);
        let before = self.calls.borrow().len();
        let result = self
            .host
            .evaluate(
                r#"await (async()=>{
            try{return {ok:true,value:await cua.computer.get_app_state({app:'Owned'})};}
            catch(error){return {ok:false,name:error.name,message:error.message,
                hasCode:Object.hasOwn(error,'code'),code:error.code};}
        })()"#,
                Duration::from_secs(5),
            )
            .unwrap();
        assert!(result.get("error").is_none(), "{result}");
        assert!(self.replies.borrow().is_empty(), "{result}");
        let calls = self.calls.borrow();
        let expected = if before == 0 {
            vec![
                "sky.setup",
                "sky.app_policy",
                "host.elicitation",
                "sky.execute",
            ]
        } else {
            vec!["sky.app_policy", "host.elicitation", "sky.execute"]
        };
        assert_eq!(calls[before..], expected);
        result["value"].clone()
    }
    fn text(&mut self) -> String {
        let result = self.request(Ok(dto()));
        assert_eq!(result["ok"], true, "{result}");
        assert_eq!(result["value"]["app"], "/owned/Native.app");
        result["value"]["text"].as_str().unwrap().to_owned()
    }
}

#[test]
fn native_app_state_formatter_errors_keep_code_absent_and_do_not_mark_keys() {
    for runtime in RuntimeBackend::available() {
        let mut owner = Owner::new(runtime);
        assert_eq!(
            owner.request(Err(Error::new(-32041, "owned provider failure"))),
            json!({"ok":false,"name":"Error","message":"owned provider failure","hasCode":true,"code":-32041})
        );
        // Root-null/scalar behavior is a current native compatibility control,
        // not a claim about the original service's root-null TypeError wording.
        for reply in [
            Value::Null,
            json!(false),
            json!(17),
            json!("root"),
            json!([]),
            json!({}),
        ] {
            assert_eq!(
                owner.request(Ok(reply)),
                json!({"ok":false,"name":"Error","message":"computer-use service did not return a screenshot","hasCode":false})
            );
        }
        let invalid = json!({"app":{"bundleIdentifier":"owned.native.formatter"},"skyshot":{"text":null,"screenshot":{"url":17}},"appSpecificInstructions":17});
        assert_eq!(
            owner.request(Ok(invalid)),
            json!({"ok":false,"name":"Error","message":"computer-use service did not return a screenshot URL","hasCode":false})
        );
        assert_eq!(owner.text(), PREFIX);
        owner
            .host
            .set_request_meta(Some(
                json!({"x-codex-turn-metadata":{"call_id":"native-formatter-next"}}),
            ))
            .unwrap();
        assert_eq!(owner.text(), TREE);
        assert_eq!(
            owner
                .calls
                .borrow()
                .iter()
                .filter(|name| name.as_str() == "sky.setup")
                .count(),
            1
        );
    }
}

#[test]
fn native_app_state_private_engine_keys_drop_with_their_host_only() {
    for runtime in RuntimeBackend::available() {
        let mut first = Owner::new(runtime);
        let mut second = Owner::new(runtime);
        assert_eq!(first.text(), PREFIX);
        assert_eq!(second.text(), PREFIX);
        drop(first);
        assert_eq!(second.text(), TREE);
        let mut replacement = Owner::new(runtime);
        assert_eq!(replacement.text(), PREFIX);
        drop(second);
        assert_eq!(replacement.text(), TREE);
    }
}
