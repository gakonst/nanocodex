use nanocodex_browser::{
    BrowserAction, BrowserActionResult, BrowserTarget,
    popcorn::{PopcornBrowser, PopcornConfig},
};

#[tokio::test]
#[ignore = "requires POPCORN_* credentials for a reachable control plane"]
async fn popcorn_session_drives_a_real_page_and_releases() {
    let config = PopcornConfig::from_env()
        .expect("set POPCORN_CONTROL_PLANE_URL, POPCORN_CLIENT_ID, and POPCORN_CLIENT_SECRET");
    let popcorn = PopcornBrowser::spawn(config).await.expect("rent session");
    eprintln!("popcorn session: {}", popcorn.session().session_id());

    // Session URLs are bearer secrets and must not survive a debug render.
    let rendered = format!("{:?}", popcorn.session());
    assert!(
        !rendered.contains("http"),
        "session URLs leaked through Debug: {rendered}"
    );

    popcorn
        .browser()
        .execute(BrowserAction::Open {
            url: "https://example.com".to_owned(),
        })
        .await
        .expect("open example.com");

    let result = popcorn
        .browser()
        .execute(BrowserAction::GetText {
            target: BrowserTarget::css("h1"),
        })
        .await
        .expect("read heading");
    let BrowserActionResult::Text { text, .. } = &result else {
        panic!("expected a text result, got {result:?}");
    };
    assert_eq!(text, "Example Domain");

    popcorn.shutdown().await.expect("release session");
}
