//! Actual Host engines entered before the installed setup prelude, only in tests.
//! No public bootstrap mode, original provider, browser, or native service is used.
use super::*;

fn host(runtime: RuntimeBackend, fail_setup: bool) -> (Host, Rc<RefCell<Vec<String>>>) {
    let calls = Rc::new(RefCell::new(Vec::new()));
    let observed = calls.clone();
    let host = Host::with_dispatch_options(
        move |method, args| {
            observed.borrow_mut().push(method.to_owned());
            match method {
                "sky.setup" if fail_setup => Err(Error::new(-32000, "owned setup unavailable")),
                "sky.setup" => Ok(json!({"target":"mac","methods":["list_apps"]})),
                "sky.execute" => {
                    assert_eq!(args["method"], "list_apps");
                    Ok(json!([]))
                }
                "browser.list" => Ok(json!([])),
                other => panic!("Unexpected provider operation: {other}"),
            }
        },
        Arc::new(AtomicBool::new(false)),
        HostOptions {
            runtime,
            ..Default::default()
        },
    )
    .unwrap();
    (host, calls)
}
fn prelude_free(host: &mut Host, code: &str) -> Value {
    let result =
        kernel::without_automatic_setup(|| host.evaluate(code, Duration::from_secs(3))).unwrap();
    assert!(result.get("error").is_none(), "{result}");
    result["value"].clone()
}
fn normal(host: &mut Host, code: &str) -> Value {
    let result = host.evaluate(code, Duration::from_secs(3)).unwrap();
    assert!(result.get("error").is_none(), "{result}");
    result["value"].clone()
}

#[test]
fn public_setup_first_options_and_pending_identity_use_the_real_host_owner() {
    let corpus: Value =
        serde_json::from_str(include_str!("../tests/oracles/cua_setup_module_cases.json")).unwrap();
    for runtime in RuntimeBackend::available() {
        for case in corpus["cases"].as_array().unwrap() {
            let (mut host, calls) = host(runtime, false);
            let code = format!(
                r#"
var {{setupCUA}} = await import('@oai/cua/tinyskyAlt');
var initialFacade=cua, initialInitialize=cua.initialize;
var first=setupCUA({});
var settled=false;first.then(()=>{{settled=true}});
var initial={{keys:Object.keys(cua).sort(),settled}};
var ignoredReads=0;
var second=setupCUA({{get browser(){{ignoredReads++;throw Error('late browser')}},get computer(){{ignoredReads++;throw Error('late computer')}}}});
var result=await first;
({{initial,samePromise:first===second,sameOwner:setupCUA===__skyreInitialize,
  sameFacade:cua===initialFacade,sameInitialize:cua.initialize===initialInitialize,
  resultUndefined:result===undefined,ignoredReads,
  browser:typeof cua.browsers,computer:typeof cua.computer,
  state:await cua.getState({{emit:false}})}})
"#,
                case["expression"].as_str().unwrap()
            );
            let browser = case["browser"].as_bool().unwrap();
            let computer = case["computer"].as_bool().unwrap();
            assert_eq!(
                prelude_free(&mut host, &code),
                json!({
                    "initial":{"keys":["initialize"],"settled":false},"samePromise":true,"sameOwner":true,
                    "sameFacade":true,"sameInitialize":true,"resultUndefined":true,"ignoredReads":0,
                    "browser":if browser {"object"} else {"undefined"},
                    "computer":if computer {"object"} else {"undefined"},
                    "state":{"apps":[],"browsers":[]}
                }),
                "{runtime:?}: {case}"
            );
            let observed = calls.borrow();
            assert_eq!(
                observed
                    .iter()
                    .filter(|v| v.as_str() == "sky.setup")
                    .count(),
                usize::from(computer)
            );
            assert_eq!(
                observed
                    .iter()
                    .filter(|v| v.as_str() == "sky.execute")
                    .count(),
                usize::from(computer)
            );
            assert_eq!(
                observed
                    .iter()
                    .filter(|v| v.as_str() == "browser.list")
                    .count(),
                usize::from(browser)
            );
            drop(observed);
            assert_eq!(
                normal(
                    &mut host,
                    r#"
var {setupCUA:laterSetup} = await import('@oai/cua/tinyskyAlt');
({samePromise:laterSetup({browser:true,computer:true})===first,
  sameOwner:laterSetup===setupCUA,browser:typeof cua.browsers,computer:typeof cua.computer})
"#
                ),
                json!({"samePromise":true,"sameOwner":true,
                "browser":if browser {"object"} else {"undefined"},
                "computer":if computer {"object"} else {"undefined"}})
            );
        }
    }
}

#[test]
fn public_setup_option_getters_order_and_cached_rejection_are_preserved() {
    for runtime in RuntimeBackend::available() {
        for rejected_field in ["browser", "computer"] {
            let (mut host, calls) = host(runtime, false);
            let code = format!(
                r#"
var {{setupCUA}} = await import('@oai/cua/tinyskyAlt');
var reads=[],failure=Error('owned option error');
var options={{get browser(){{reads.push('browser');if('{}'==='browser')throw failure;return true}},
  get computer(){{reads.push('computer');if('{}'==='computer')throw failure;return true}}}};
var syncThrow=false,first;try{{first=setupCUA(options)}}catch(e){{syncThrow=true}}
var observed;try{{await first}}catch(e){{observed=e}}
var replacementReads=0;
var second=setupCUA({{get browser(){{replacementReads++;return false}},get computer(){{replacementReads++;return false}}}});
var repeated;try{{await second}}catch(e){{repeated=e}}
({{syncThrow,reads,samePromise:first===second,sameError:observed===failure&&repeated===failure,
  replacementReads,keys:Object.keys(cua).sort(),agent:typeof globalThis.agent}})
"#,
                rejected_field, rejected_field
            );
            assert_eq!(
                prelude_free(&mut host, &code),
                json!({"syncThrow":false,
                "reads":if rejected_field=="browser" {vec!["browser"]}else{vec!["browser","computer"]},
                "samePromise":true,"sameError":true,"replacementReads":0,"keys":["initialize"],"agent":"undefined"})
            );
            assert!(calls.borrow().is_empty());
            // A subsequent ordinary cell encounters the same rejection at its
            // mandatory prelude; no new options or provider calls are admitted.
            let later = host
                .evaluate("throw Error('body must not run')", Duration::from_secs(3))
                .unwrap();
            assert_eq!(
                later["exceptionMessage"], "owned option error",
                "{runtime:?}: {later}"
            );
            assert!(calls.borrow().is_empty());
        }
        let (mut host, calls) = host(runtime, false);
        assert_eq!(
            prelude_free(
                &mut host,
                r#"
var {setupCUA} = await import('@oai/cua/tinyskyAlt');
var syncThrow=false,p;try{p=setupCUA(null)}catch(e){syncThrow=true}
var error;try{await p}catch(e){error=e}
var q=setupCUA({browser:false,computer:false});
var again;try{await q}catch(e){again=e}
({syncThrow,type:error.name,samePromise:p===q,sameError:error===again,keys:Object.keys(cua)})
"#
            ),
            json!({"syncThrow":false,"type":"TypeError","samePromise":true,"sameError":true,"keys":["initialize"]})
        );
        assert!(calls.borrow().is_empty());
    }
}

#[test]
fn public_setup_reads_options_before_native_provider_work_and_keeps_pending_selection() {
    for runtime in RuntimeBackend::available() {
        let (mut host, calls) = host(runtime, false);
        assert_eq!(
            prelude_free(
                &mut host,
                r#"
var {setupCUA} = await import('@oai/cua/tinyskyAlt');
var reads=[],opts={get browser(){reads.push('browser');return false},
 get computer(){reads.push('computer');return true}};
var p=setupCUA(opts);
var before={reads:[...reads],keys:Object.keys(cua),agent:typeof globalThis.agent};
var q=setupCUA({browser:true,computer:false});
var value=await p;
({before,samePromise:p===q,resultUndefined:value===undefined,
  browser:typeof cua.browsers,computer:typeof cua.computer,reads})
"#
            ),
            json!({"before":{"reads":["browser","computer"],"keys":["initialize"],"agent":"undefined"},
            "samePromise":true,"resultUndefined":true,"browser":"undefined","computer":"object","reads":["browser","computer"]})
        );
        assert_eq!(&*calls.borrow(), &["sky.setup"]);
    }
}

#[test]
fn public_setup_native_computer_refusal_is_not_assimilated_as_a_thenable() {
    for runtime in RuntimeBackend::available() {
        let (mut host, calls) = host(runtime, true);
        assert_eq!(
            prelude_free(
                &mut host,
                r#"
var {setupCUA} = await import('@oai/cua/tinyskyAlt');
var p=setupCUA({browser:false,computer:true}),result=await p;
var failure;try{await cua.computer.list_apps()}catch(e){failure=e.message}
({resultUndefined:result===undefined,samePromise:p===setupCUA(),
  computer:typeof cua.computer,browser:typeof cua.browsers,failure})
"#
            ),
            json!({"resultUndefined":true,"samePromise":true,"computer":"object","browser":"undefined","failure":"owned setup unavailable"})
        );
        assert_eq!(&*calls.borrow(), &["sky.setup"]);
    }
}

#[test]
fn public_setup_hosts_keep_independent_first_options_and_module_caches() {
    for runtime in RuntimeBackend::available() {
        let (mut a, a_calls) = host(runtime, false);
        let (mut b, b_calls) = host(runtime, false);
        assert_eq!(
            prelude_free(
                &mut a,
                "var {setupCUA} = await import('@oai/cua/tinyskyAlt');var p=setupCUA({browser:false,computer:false});await p;Object.keys(cua).sort()"
            ),
            json!(["getState", "initialize"])
        );
        assert_eq!(
            prelude_free(
                &mut b,
                "var {setupCUA} = await import('@oai/cua/tinyskyAlt');var p=setupCUA({browser:false});await p;typeof cua.computer"
            ),
            json!("object")
        );
        assert_eq!(
            normal(&mut a, "[p===__skyreInitialize(),typeof cua.computer]"),
            json!([true, "undefined"])
        );
        assert_eq!(
            normal(&mut b, "[p===__skyreInitialize(),typeof cua.computer]"),
            json!([true, "object"])
        );
        assert!(a_calls.borrow().is_empty());
        assert_eq!(&*b_calls.borrow(), &["sky.setup"]);
    }
}

#[test]
fn public_setup_promise_survives_an_outstanding_native_provider_reply() {
    for runtime in RuntimeBackend::available() {
        let (entered, waiting) = std::sync::mpsc::sync_channel(1);
        let (release, released) = std::sync::mpsc::sync_channel(1);
        let execution = std::thread::spawn(move || {
            let mut count = 0;
            let mut host = Host::with_dispatch_options(
                move |method, _| {
                    assert_eq!(method, "sky.setup");
                    count += 1;
                    assert_eq!(
                        count, 1,
                        "setup must not restart while its reply is pending"
                    );
                    entered.send(()).unwrap();
                    released
                        .recv_timeout(Duration::from_secs(2))
                        .expect("owned reply gate release");
                    Ok(json!({"target":"mac","methods":["list_apps"]}))
                },
                Arc::new(AtomicBool::new(false)),
                HostOptions {
                    runtime,
                    ..Default::default()
                },
            )
            .unwrap();
            prelude_free(
                &mut host,
                r#"
var {setupCUA} = await import('@oai/cua/tinyskyAlt');
var p=setupCUA({browser:false}),settled=false;p.then(()=>{settled=true});
var before={settled,keys:Object.keys(cua)};
var q=setupCUA({computer:false,browser:true});
await p;
({before,samePromise:p===q,settled,browser:typeof cua.browsers,computer:typeof cua.computer})
"#,
            )
        });
        let reached = waiting.recv_timeout(Duration::from_secs(12));
        // Always release/join the owned fixture, including an assertion failure.
        let _ = release.send(());
        let value = execution.join().expect("owned Host thread");
        assert!(
            reached.is_ok(),
            "native provider wait was not reached: {runtime:?}"
        );
        assert_eq!(
            value,
            json!({"before":{"settled":false,"keys":["initialize"]},
            "samePromise":true,"settled":true,"browser":"undefined","computer":"object"})
        );
    }
}

#[test]
fn public_setup_getter_reentry_preserves_outer_cache_and_inner_options() {
    // Original globals assigns its cache only after the factory returns a
    // promise. A getter may finish a nested setup call before that assignment.
    for runtime in RuntimeBackend::available() {
        for different_options in [false, true] {
            let (mut host, calls) = host(runtime, false);
            let code = format!(
                r#"
var {{setupCUA}} = await import('@oai/cua/tinyskyAlt');
var reentryState={{reads:[],lateReads:0,reentries:0,duringInnerCache:false}};
var lateOptions={{get browser(){{reentryState.lateReads++;throw Error('late browser')}},
  get computer(){{reentryState.lateReads++;throw Error('late computer')}}}};
var inner,outer;
var reenter=()=>{{
  if(++reentryState.reentries!==1)throw Error('unexpected repeated reentry');
  inner=setupCUA({{get browser(){{reentryState.reads.push('inner.browser');return false}},
    get computer(){{reentryState.reads.push('inner.computer');return {different_options}}}}});
  inner.catch(()=>{{}});
  reentryState.duringInnerCache=setupCUA(lateOptions)===inner;
}};
outer=setupCUA({{get browser(){{reentryState.reads.push('outer.browser');if(!{different_options})reenter();return {different_options}}},
  get computer(){{reentryState.reads.push('outer.computer');if({different_options})reenter();return false}}}});
outer.catch(()=>{{}});
var initialKeys=Object.keys(cua).sort();
var cachedBefore=setupCUA(lateOptions)===outer;
var outcomes=await Promise.allSettled([outer,inner]);
({{differentPromises:outer!==inner,duringInnerCache:reentryState.duringInnerCache,
  cachedBefore,cachedAfter:setupCUA(lateOptions)===outer,initialKeys,
  reads:reentryState.reads,lateReads:reentryState.lateReads,reentries:reentryState.reentries,
  statuses:outcomes.map(value=>value.status),undefinedValues:outcomes.map(value=>value.status==='fulfilled'&&value.value===undefined),
  keys:Object.keys(cua).sort(),browser:typeof cua.browsers,computer:typeof cua.computer}})
"#
            );
            assert_eq!(
                prelude_free(&mut host, &code),
                json!({
                    "differentPromises":true,"duringInnerCache":true,
                    "cachedBefore":true,"cachedAfter":true,"initialKeys":["initialize"],
                    "reads":if different_options {
                        vec!["outer.browser","outer.computer","inner.browser","inner.computer"]
                    } else {
                        vec!["outer.browser","inner.browser","inner.computer","outer.computer"]
                    },
                    "lateReads":0,"reentries":1,"statuses":["fulfilled","fulfilled"],
                    "undefinedValues":[true,true],
                    "keys":if different_options {
                        vec!["browsers","computer","createBrowserTab","getApp","getBrowser","getState","getTab","initialize","listApps","listBrowsers","listTabs"]
                    } else {vec!["getState","initialize"]},
                    "browser":if different_options {"object"} else {"undefined"},
                    "computer":if different_options {"object"} else {"undefined"}
                }),
                "{runtime:?}, different_options={different_options}"
            );
            assert_eq!(
                normal(
                    &mut host,
                    r#"
var {setupCUA:laterSetup} = await import('@oai/cua/tinyskyAlt');
({sameOwner:laterSetup===setupCUA,samePromise:laterSetup(lateOptions)===outer,
  lateReads:reentryState.lateReads,browser:typeof cua.browsers,computer:typeof cua.computer})
"#,
                ),
                json!({"sameOwner":true,"samePromise":true,"lateReads":0,
                    "browser":if different_options {"object"}else{"undefined"},
                    "computer":if different_options {"object"}else{"undefined"}}),
                "{runtime:?}, different_options={different_options}"
            );
            assert_eq!(
                &*calls.borrow(),
                &if different_options {
                    vec!["sky.setup".to_owned()]
                } else {
                    vec![]
                },
                "only the admitted inner computer setup may call the provider"
            );
        }
    }
}

#[test]
fn public_setup_getter_reentry_preserves_independent_rejections() {
    for runtime in RuntimeBackend::available() {
        for outer_rejects in [false, true] {
            let (mut host, calls) = host(runtime, false);
            let code = format!(
                r#"
var {{setupCUA}} = await import('@oai/cua/tinyskyAlt');
var reentryState={{reads:[],lateReads:0,reentries:0,duringInnerCache:false,bodyRuns:0}};
var ownedFailure=Error('owned reentry option rejection');
var lateOptions={{get browser(){{reentryState.lateReads++;throw Error('late browser')}},
  get computer(){{reentryState.lateReads++;throw Error('late computer')}}}};
var inner,outer;
outer=setupCUA({{get browser(){{
  reentryState.reads.push('outer.browser');
  if(++reentryState.reentries!==1)throw Error('unexpected repeated reentry');
  inner=setupCUA({{get browser(){{reentryState.reads.push('inner.browser');return false}},
    get computer(){{reentryState.reads.push('inner.computer');if(!{outer_rejects})throw ownedFailure;return false}}}});
  inner.catch(()=>{{}});
  reentryState.duringInnerCache=setupCUA(lateOptions)===inner;
  if({outer_rejects})throw ownedFailure;return false;
}},get computer(){{reentryState.reads.push('outer.computer');return false}}}});
outer.catch(()=>{{}});
var cachedBefore=setupCUA(lateOptions)===outer;
var outcomes=await Promise.allSettled([outer,inner]);
({{differentPromises:outer!==inner,duringInnerCache:reentryState.duringInnerCache,
  cachedBefore,cachedAfter:setupCUA(lateOptions)===outer,
  reads:reentryState.reads,lateReads:reentryState.lateReads,reentries:reentryState.reentries,
  statuses:outcomes.map(value=>value.status),
  ownedErrors:outcomes.map(value=>value.status==='rejected'?value.reason===ownedFailure:null),
  undefinedValues:outcomes.map(value=>value.status==='fulfilled'?value.value===undefined:null),
  keys:Object.keys(cua).sort(),browser:typeof cua.browsers,computer:typeof cua.computer}})
"#
            );
            assert_eq!(
                prelude_free(&mut host, &code),
                json!({"differentPromises":true,"duringInnerCache":true,"cachedBefore":true,"cachedAfter":true,
                    "reads":if outer_rejects {
                        vec!["outer.browser","inner.browser","inner.computer"]
                    } else {vec!["outer.browser","inner.browser","inner.computer","outer.computer"]},
                    "lateReads":0,"reentries":1,
                    "statuses":if outer_rejects {vec!["rejected","fulfilled"]}else{vec!["fulfilled","rejected"]},
                    "ownedErrors":if outer_rejects {json!([true,null])}else{json!([null,true])},
                    "undefinedValues":if outer_rejects {json!([null,true])}else{json!([true,null])},
                    "keys":["getState","initialize"],"browser":"undefined","computer":"undefined"}),
                "{runtime:?}, outer_rejects={outer_rejects}"
            );
            if outer_rejects {
                let later = host
                    .evaluate("reentryState.bodyRuns++", Duration::from_secs(3))
                    .unwrap();
                assert_eq!(
                    later["exceptionMessage"], "owned reentry option rejection",
                    "{runtime:?}: the cached outer rejection must stop the next prelude: {later}"
                );
                assert_eq!(
                    prelude_free(
                        &mut host,
                        "[reentryState.bodyRuns,setupCUA(lateOptions)===outer,reentryState.lateReads]"
                    ),
                    json!([0, true, 0])
                );
            } else {
                assert_eq!(
                    normal(
                        &mut host,
                        "[setupCUA(lateOptions)===outer,reentryState.lateReads,typeof cua.getState]"
                    ),
                    json!([true, 0, "function"])
                );
            }
            assert!(calls.borrow().is_empty(), "{runtime:?}: disabled providers");
        }
    }
}

#[path = "runtime_cua_setup_publication_tests.rs"]
mod publication_tests;
