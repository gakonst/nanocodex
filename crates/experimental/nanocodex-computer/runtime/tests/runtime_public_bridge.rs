//! Public-kernel inventory is captured from the installed kernel with one inert
//! registered service; private behavior is exercised only through CUA adapters.
use serde_json::{Value, json};
use skyre::runtime::{Host, HostOptions, RuntimeBackend};
use std::sync::{Arc, atomic::AtomicBool};
use std::time::Duration;
fn host(backend: RuntimeBackend) -> Host {
    Host::with_dispatch_options(
        |method, _| match method {
            "sky.setup" => Ok(json!({"target":"mac","methods":["get_app_state","list_apps"]})),
            "sky.execute" => Ok(json!([])),
            other => panic!("Unapproved private call reached host: {other}"),
        },
        Arc::new(AtomicBool::new(false)),
        HostOptions {
            runtime: backend,
            ..Default::default()
        },
    )
    .unwrap()
}
fn evaluate(host: &mut Host, code: &str) -> Value {
    host.evaluate(code, Duration::from_secs(3)).unwrap()
}
fn written(result: &Value) -> Value {
    result["outputs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["channel"] == "output")
        .unwrap()["value"]
        .clone()
}
#[test]
fn public_bridge_inventory_descriptors_and_freeze_match_installed_kernel() {
    let original: Value = serde_json::from_str(include_str!(
        "oracles/runtime_public_bridge_2026-09-07.json"
    ))
    .unwrap();
    for backend in RuntimeBackend::available() {
        let mut host = host(backend);
        let result = evaluate(&mut host, original["code"].as_str().unwrap());
        let actual: Value = serde_json::from_str(written(&result).as_str().unwrap()).unwrap();
        assert_eq!(actual, original["expected"], "{backend:?}");
        let result = evaluate(
            &mut host,
            "[Reflect.set(nodeRepl,'setResponseMeta',()=>{}),Reflect.deleteProperty(globalThis,'nodeRepl'),Reflect.defineProperty(nodeRepl,'withSuspendedTimeout',{value:()=>{}}),typeof nodeRepl.createElicitation]",
        );
        assert_eq!(
            result["value"],
            json!([false, false, false, "undefined"]),
            "{result}"
        );
    }
}
#[test]
fn model_cannot_recover_privileged_callbacks_or_disable_its_deadline() {
    for backend in RuntimeBackend::available() {
        let mut host = host(backend);
        let result = evaluate(
            &mut host,
            "[Reflect.ownKeys(globalThis).filter(k=>typeof k==='string'&&(/^__skyre_(rpc|suspend_timeout|response_meta)$/.test(k)||k==='__skyreTakePrivateBridge')),['createElicitation','withSuspendedTimeout','setResponseMeta'].map(k=>typeof nodeRepl[k])]",
        );
        assert_eq!(
            result["value"],
            json!([[], ["undefined", "undefined", "undefined"]]),
            "{result}"
        );
        for code in [
            "nodeRepl.setResponseMeta({forged:true})",
            "nodeRepl.createElicitation({message:'forged'})",
            "__skyre_response_meta('{}')",
            "__skyre_rpc('host.elicitation','{}')",
            "__skyreTakePrivateBridge()",
        ] {
            let result = evaluate(&mut host, code);
            assert!(result.get("error").is_some(), "{code}: {result}");
            assert_eq!(result["responseMeta"], json!({}), "{code}: {result}");
        }
        let result=host.evaluate("try{nodeRepl.withSuspendedTimeout(()=>{})}catch{};try{__skyre_suspend_timeout(true)}catch{};await new Promise(resolve=>setTimeout(resolve,200));",Duration::from_millis(50)).unwrap();
        assert!(
            result["error"]["message"]
                .as_str()
                .unwrap()
                .contains("timed out"),
            "{result}"
        );
    }
}
#[test]
fn trusted_metadata_and_writer_receiver_do_not_leak_through_model_intrinsics() {
    for backend in RuntimeBackend::available() {
        let mut host = host(backend);
        evaluate(&mut host, "0");
        let result = evaluate(
            &mut host,
            r#"
          let leakedPrivateBridge=false;
          const savedBind=Function.prototype.bind,savedPush=Array.prototype.push,savedToJSON=Object.getOwnPropertyDescriptor(Object.prototype,'toJSON');
          Function.prototype.bind=function(...args){if(args[0]&&typeof args[0].withSuspendedTimeout==='function')leakedPrivateBridge=true;return Reflect.apply(savedBind,this,args)};
          Object.prototype.toJSON=function(){return {forged:true}};
          Array.prototype.push=function(...args){for(const value of args)if(value&&typeof value==='object'&&Object.hasOwn(value,'codex/toolSurface'))value['codex/toolSurface']={forged:true};return Reflect.apply(savedPush,this,args)};
          try{await cua.listApps()}finally{Function.prototype.bind=savedBind;Array.prototype.push=savedPush;if(savedToJSON)Object.defineProperty(Object.prototype,'toJSON',savedToJSON);else delete Object.prototype.toJSON}
          leakedPrivateBridge
        "#,
        );
        assert!(result.get("error").is_none(), "{result}");
        assert_eq!(result["value"], false, "{result}");
        assert_eq!(
            result["responseMeta"],
            json!({"codex/toolSurface":{"app":null,"kind":"computerUse"}}),
            "{result}"
        );
    }
}

#[test]
fn inherited_then_cannot_replace_private_policy_or_declined_approval() {
    for backend in RuntimeBackend::available() {
        let mut host=Host::with_dispatch_options(|method,args|match method {
            "sky.setup"=>Ok(json!({"target":"mac","methods":["get_app_state"]})),
            "sky.app_policy"=>Ok(json!({"decision":"allowed","allowPersistentApproval":false,"target":{"bundleIdentifier":"owned.fixture","displayName":"Owned fixture","appPath":"/owned/Fixture.app","risk":"low"}})),
            "host.elicitation"=>{
                assert_eq!(args["meta"]["tool_params"]["app"],"owned.fixture");
                Ok(json!({"action":"decline"}))
            },
            other=>panic!("Declined operation reached {other}"),
        },Arc::new(AtomicBool::new(false)),HostOptions{runtime:backend,..Default::default()}).unwrap();
        evaluate(&mut host, "0");
        let result = evaluate(
            &mut host,
            r#"
          let interceptedPrivateRecord=0,declineMessage;
          Object.defineProperty(Object.prototype,'then',{configurable:true,get(){
            if(!Object.hasOwn(this,'target')&&!Object.hasOwn(this,'action'))return undefined;
            return resolve=>{
              interceptedPrivateRecord++;
              Object.defineProperty(this,'then',{value:undefined});
              if(this.target)this.target.bundleIdentifier='forged';
              if(this.action)this.action='accept';
              resolve(this);
            };
          }});
          try{await cua.getApp('fixture')}catch(error){declineMessage=error.message}
          finally{delete Object.prototype.then}
          [interceptedPrivateRecord,declineMessage]
        "#,
        );
        assert_eq!(
            result["value"],
            json!([0, "Computer Use was not approved to use Owned fixture"]),
            "{backend:?}: {result}"
        );
        assert_eq!(
            result["responseMeta"]["codex/toolSurface"]["app"]["appId"],
            "owned.fixture"
        );
    }
}
