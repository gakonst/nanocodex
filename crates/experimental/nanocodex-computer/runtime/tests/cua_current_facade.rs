//! Current installed CUA 0.2.5 factory contract, captured with inert providers.
use rquickjs::{Context, Runtime};
use serde_json::Value;

#[test]
fn current_facade_matches_installed_factory_in_quickjs() {
    // Use an isolated QuickJS realm so fixture output sinks can be instrumented
    // without weakening the production Host's protected nodeRepl binding.
    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();
    context.with(|ctx| {
        ctx.eval::<(),_>(include_str!("../src/facade.js")).unwrap();
        ctx.eval::<(),_>(format!(
            "globalThis.__testCreateCUA=__skyreCreateCUA;{}.then(value=>globalThis.result=JSON.stringify(value),error=>globalThis.failure=String(error.stack));",
            include_str!("cua_current_facade_cases.js").trim()
        )).unwrap();
    });
    let mut jobs = 0;
    while runtime.is_job_pending() {
        runtime.execute_pending_job().unwrap();
        jobs += 1;
        assert!(jobs < 10000, "inert fixture failed to settle");
    }
    context.with(|ctx| {
        let failure: Option<String> = ctx.globals().get("failure").unwrap();
        assert!(failure.is_none(), "{failure:?}");
        let result: String = ctx.globals().get("result").unwrap();
        let actual: Value = serde_json::from_str(&result).unwrap();
        let expected: Value =
            serde_json::from_str(include_str!("oracles/cua_current_facade.json")).unwrap();
        assert_eq!(actual, expected);
    });
}

#[test]
fn current_window_facade_matches_installed_factory_in_quickjs() {
    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();
    context.with(|ctx| {
        ctx.eval::<(), _>(include_str!("../src/facade.js")).unwrap();
        ctx.eval::<(), _>(format!(
            "globalThis.__testCreateCUA=__skyreCreateCUA;{}.then(value=>globalThis.result=JSON.stringify(value),error=>globalThis.failure=String(error.stack));",
            include_str!("cua_current_windows_cases.js").trim()
        )).unwrap();
    });
    let mut jobs = 0;
    while runtime.is_job_pending() {
        runtime.execute_pending_job().unwrap();
        jobs += 1;
        assert!(jobs < 10000, "inert window fixture failed to settle");
    }
    context.with(|ctx| {
        let failure: Option<String> = ctx.globals().get("failure").unwrap();
        assert!(failure.is_none(), "{failure:?}");
        let result: String = ctx.globals().get("result").unwrap();
        let actual: Value = serde_json::from_str(&result).unwrap();
        let expected: Value =
            serde_json::from_str(include_str!("oracles/cua_current_windows.json")).unwrap();
        assert_eq!(actual, expected);
    });
}

#[test]
fn current_tab_references_match_installed_factory_with_runtime_url_support() {
    use skyre::runtime::{Host, HostOptions, RuntimeBackend};
    use std::{
        sync::{Arc, atomic::AtomicBool},
        time::Duration,
    };
    for runtime in RuntimeBackend::available() {
        let mut options = HostOptions {
            runtime,
            ..Default::default()
        };
        options
            .env
            .insert("CUA_REPL_ENABLED_SURFACES".into(), "browser".into());
        let mut host = Host::with_dispatch_options(
            |method, _| panic!("Unexpected provider call in reference fixture: {method}"),
            Arc::new(AtomicBool::new(false)),
            options,
        )
        .unwrap();
        let code = format!(
            "globalThis.__testCreateCUA=__skyreCreateCUA; await {}",
            include_str!("cua_current_references_cases.js")
        );
        let result = host.evaluate(&code, Duration::from_secs(3)).unwrap();
        assert!(result.get("error").is_none(), "{result}");
        let expected: Value =
            serde_json::from_str(include_str!("oracles/cua_current_references.json")).unwrap();
        assert_eq!(result["value"], expected);
    }
}

#[test]
fn current_browser_input_decorator_matches_installed_factory_in_quickjs() {
    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();
    context.with(|ctx| {
        ctx.eval::<(), _>(include_str!("../src/facade.js")).unwrap();
        ctx.eval::<(), _>(include_str!("../src/browser_facade.js")).unwrap();
        ctx.eval::<(), _>(r#"
            globalThis.__testCreateCUA=__skyreCreateCUA;
            globalThis.__testDecorateTab=source=>{
                const tab=__skyreBrowserFacade({rpc:async()=>{throw Error('Unexpected provider call');}}).tab('fixture',source.id);
                Object.assign(tab.ax,source.ax);return tab;
            };
        "#).unwrap();
        ctx.eval::<(), _>(format!(
            "{}.then(value=>globalThis.result=JSON.stringify(value),error=>globalThis.failure=String(error.stack));",
            include_str!("cua_current_browser_input_cases.js").trim()
        )).unwrap();
    });
    let mut jobs = 0;
    while runtime.is_job_pending() {
        runtime.execute_pending_job().unwrap();
        jobs += 1;
        assert!(jobs < 10000, "inert input fixture failed to settle");
    }
    context.with(|ctx| {
        let failure: Option<String> = ctx.globals().get("failure").unwrap();
        assert!(failure.is_none(), "{failure:?}");
        let result: String = ctx.globals().get("result").unwrap();
        let actual: Value = serde_json::from_str(&result).unwrap();
        let expected: Value =
            serde_json::from_str(include_str!("oracles/cua_current_browser_input.json")).unwrap();
        assert_eq!(actual, expected);
    });
}

#[test]
fn current_browser_input_preserves_target_in_provider_envelope() {
    let runtime = Runtime::new().unwrap();
    let context = Context::full(&runtime).unwrap();
    context.with(|ctx| {
        ctx.eval::<(), _>(include_str!("../src/browser_facade.js")).unwrap();
        ctx.eval::<(), _>(r#"
            globalThis.calls=[];
            globalThis.tab=__skyreBrowserFacade({rpc:async(method,args)=>calls.push({method,args})}).tab('fixture','tab');
            (async()=>{
              await tab.paste(4,'text');await tab.pressKey(null,'Return');await tab.typeText(0,'typed');
              await tab.paste(5,'<b>html</b>',{format:'html'});
              globalThis.result=JSON.stringify(calls);
            })().catch(error=>globalThis.failure=String(error.stack));
        "#).unwrap();
    });
    while runtime.is_job_pending() {
        runtime.execute_pending_job().unwrap();
    }
    context.with(|ctx| {
        let failure: Option<String>=ctx.globals().get("failure").unwrap();
        assert!(failure.is_none(), "{failure:?}");
        let result: String=ctx.globals().get("result").unwrap();
        let actual: Value=serde_json::from_str(&result).unwrap();
        assert_eq!(actual,serde_json::json!([
            {"method":"browser.tab_ax_action","args":{"browser":"fixture","tab":"tab","action":{"kind":"paste","element_index":4,"text":"text"}}},
            {"method":"browser.tab_ax_action","args":{"browser":"fixture","tab":"tab","action":{"kind":"press_key","element_index":null,"key":"Return"}}},
            {"method":"browser.tab_ax_action","args":{"browser":"fixture","tab":"tab","action":{"kind":"type_text","element_index":0,"text":"typed"}}},
            {"method":"browser.tab_ax_action","args":{"browser":"fixture","tab":"tab","action":{"kind":"paste","element_index":5,"text":"<b>html</b>","format":"html"}}}
        ]));
    });
}
