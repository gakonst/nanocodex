//! Current installed CUA 0.2.4 factory contract, captured with inert providers.
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
