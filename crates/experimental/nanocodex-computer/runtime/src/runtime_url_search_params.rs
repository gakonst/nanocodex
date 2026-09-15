use rquickjs::{Ctx, prelude::Func};

/// WHATWG form decoding uses the same Rust URL dependency as the URL adapter.
/// No filesystem, network, browser or native provider is involved.
pub fn install(ctx: &Ctx<'_>) -> rquickjs::Result<()> {
    ctx.globals().set(
        "__skyre_form_decode",
        Func::from(|input: String| -> String {
            let pairs: Vec<_> = url::form_urlencoded::parse(input.as_bytes()).collect();
            serde_json::to_string(&pairs).expect("form pairs serialize as strings")
        }),
    )?;
    ctx.eval::<(), _>(include_str!("runtime_url_search_params.js"))
}
