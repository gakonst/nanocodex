#![cfg(any(
    all(target_os = "linux", not(target_env = "musl")),
    all(target_os = "macos", target_arch = "aarch64")
))]

use std::{ffi::OsString, path::PathBuf};

use nanocodex_browser::{
    BrowserAction, BrowserActionResult, BrowserTarget,
    vm::{BrowserVm, BrowserVmBuilder},
};

const PROOF_PAGE: &str =
    "data:text/html,<main><h1 id='status'>Ready</h1><button>Save</button></main>";

fn live_builder() -> Option<BrowserVmBuilder> {
    let mut builder = BrowserVm::builder(
        PathBuf::from(std::env::var_os("NANOCODEX_BROWSER_VM_ROOTFS")?),
        PathBuf::from(std::env::var_os("NANOCODEX_BROWSER_VM_VMM")?),
        PathBuf::from(std::env::var_os("NANOCODEX_BROWSER_VM_GVPROXY")?),
    );
    if let Some(firmware) = std::env::var_os("NANOCODEX_BROWSER_VM_FIRMWARE") {
        builder = builder.firmware_directory(firmware);
    }
    if let Some(arguments) = std::env::var_os("NANOCODEX_BROWSER_VM_VMM_ARGS") {
        let arguments = serde_json::from_str::<Vec<String>>(&arguments.to_string_lossy()).ok()?;
        builder = builder.vmm_args(arguments.into_iter().map(OsString::from));
    }
    Some(builder)
}

#[tokio::test]
#[ignore = "requires a prepared browser image, VMM, gvproxy, and optional libkrun firmware"]
async fn headed_browser_vm_runs_typed_actions_and_reaps() {
    let builder = live_builder().expect(
        "set NANOCODEX_BROWSER_VM_ROOTFS, NANOCODEX_BROWSER_VM_VMM, and \
         NANOCODEX_BROWSER_VM_GVPROXY",
    );
    let vm = builder.spawn().await.expect("spawn browser VM");
    vm.browser()
        .execute(BrowserAction::Open {
            url: PROOF_PAGE.to_owned(),
        })
        .await
        .expect("open proof page");

    let text = vm
        .browser()
        .execute(BrowserAction::GetText {
            target: BrowserTarget::css("#status"),
        })
        .await
        .expect("read proof text");
    assert!(matches!(
        text,
        BrowserActionResult::Text { ref text, .. } if text == "Ready"
    ));

    let snapshot = vm
        .browser()
        .execute(BrowserAction::Snapshot {
            interactive: true,
            compact: true,
            depth: None,
            selector: None,
            include_urls: false,
        })
        .await
        .expect("snapshot proof page");
    assert!(matches!(
        snapshot,
        BrowserActionResult::Snapshot { ref refs, .. } if !refs.is_empty()
    ));

    let screenshot = vm
        .browser()
        .execute(BrowserAction::Screenshot {
            full_page: false,
            annotate: false,
            target: None,
        })
        .await
        .expect("capture screenshot");
    assert!(matches!(
        screenshot,
        BrowserActionResult::Screenshot {
            image: Some(ref image),
            ..
        } if image.path.is_file()
    ));
    vm.shutdown().await.expect("shutdown browser VM");
}
