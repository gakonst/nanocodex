fn main() {
    println!("cargo:rerun-if-env-changed=CARGO_CFG_TARGET_OS");
    println!("cargo:rerun-if-env-changed=CARGO_CFG_TARGET_ENV");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("gnu")
    {
        // Opus enables GCC stack protection. Include its runtime statically so
        // the standalone Windows Hand does not need an extra libssp DLL.
        println!("cargo:rustc-link-lib=static:+whole-archive=ssp");
    }
}
