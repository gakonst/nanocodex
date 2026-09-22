//! Three-second system-output capture probe; emits counts, never audio contents.
#[cfg(target_os = "windows")]
fn main() -> Result<(), nanocodex_hand::Error> {
    use std::{
        io::Write,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        },
        time::{Duration, Instant},
    };
    #[derive(Default)]
    struct Counts {
        bytes: usize,
        nonzero_samples: usize,
    }
    struct Sink(Arc<Mutex<Counts>>);
    impl Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let mut counts = self.0.lock().unwrap();
            counts.bytes += bytes.len();
            counts.nonzero_samples += bytes
                .chunks_exact(2)
                .filter(|sample| sample[0] != 0 || sample[1] != 0)
                .count();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let stop = Arc::new(AtomicBool::new(false));
    let counts = Arc::new(Mutex::new(Counts::default()));
    let sink = Sink(counts.clone());
    let worker_stop = stop.clone();
    let began = Instant::now();
    let worker = std::thread::spawn(move || nanocodex_hand::capture_audio(sink, worker_stop));
    std::thread::sleep(Duration::from_secs(3));
    stop.store(true, Ordering::Release);
    worker
        .join()
        .map_err(|_| std::io::Error::other("audio capture worker panicked"))??;
    let counts = counts.lock().unwrap();
    println!(
        "{}",
        serde_json::json!({"bytes":counts.bytes,"nonzero_samples":counts.nonzero_samples,
        "pcm_seconds":counts.bytes as f64/192_000.0,"elapsed_seconds":began.elapsed().as_secs_f64(),"sample_rate":48000,"channels":2,"format":"s16le"})
    );
    Ok(())
}
#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("This audio probe requires an interactive Windows desktop with a render endpoint.");
}
