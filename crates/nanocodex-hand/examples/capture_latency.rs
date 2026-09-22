//! Observe the local display without saving or emitting image contents.
//! Requires the invoking application's existing OS screen-recording permission.
#[cfg(any(target_os = "macos", target_os = "windows"))]
use std::time::Instant;

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter("nanocodex_hand=debug")
        .init();
    let mut elapsed = Vec::new();
    for sample in 0..10 {
        let began = Instant::now();
        let frame = nanocodex_hand::request(serde_json::json!({"action":"observe"}))?;
        elapsed.push(began.elapsed().as_secs_f64() * 1000.0);
        println!(
            "{}",
            serde_json::json!({
                "sample": sample, "elapsed_ms": elapsed[sample],
                "status": frame["status"], "width": frame["width"], "height": frame["height"],
                "encoded_bytes": frame["jpeg"].as_str().map(str::len),
            })
        );
    }
    let cold_ms = elapsed.remove(0);
    elapsed.sort_by(f64::total_cmp);
    println!(
        "{}",
        serde_json::json!({
            "summary": true, "cold_ms": cold_ms,
            "warm_median_ms": elapsed[elapsed.len() / 2],
            "warm_max_ms": elapsed[elapsed.len() - 1],
            "warm_samples": elapsed.len(),
        })
    );
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn main() {
    eprintln!("This capture probe requires macOS or Windows.");
}
