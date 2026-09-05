use std::{
    io::Cursor,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use chromiumoxide::{Page, cdp::browser_protocol::page::Viewport, page::ScreenshotParams};
use image::{DynamicImage, ImageBuffer, Rgba};
use tokio::task::JoinHandle;
use tracing::warn;

use crate::{
    BrowserImageArtifact, BrowserVisualAnomaly, BrowserVisualAnomalyKind, BrowserVisualDiff,
    BrowserVisualTrace,
};

use super::BrowserError;

const DEFAULT_PIXEL_THRESHOLD: u8 = 16;
pub(crate) const MAX_IMAGE_ARTIFACT_BYTES: u64 = 32 * 1024 * 1024;
const MAX_IMAGE_ARTIFACT_PIXELS: u64 = 32 * 1024 * 1024;
const FLASH_CHANGE_RATIO: f64 = 0.35;
const RETURN_CHANGE_RATIO: f64 = 0.08;
const BLANK_PIXEL_RATIO: f64 = 0.985;

pub(super) struct VisualTraceState {
    started: Instant,
    frames: Arc<StdMutex<Vec<TraceFrame>>>,
    dropped: Arc<AtomicU64>,
    task: Option<JoinHandle<()>>,
}

impl Drop for VisualTraceState {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

#[derive(Clone)]
struct TraceFrame {
    elapsed: Duration,
    path: PathBuf,
}

#[derive(Clone)]
struct FrameAnalysis {
    path: PathBuf,
    elapsed: Duration,
    image: DynamicImage,
    luminance: f64,
    blank_pixel_ratio: f64,
}

pub(super) async fn capture(
    page: &Page,
    output_dir: &Path,
    artifact_id: String,
    full_page: bool,
    clip: Option<Viewport>,
) -> Result<BrowserImageArtifact, BrowserError> {
    let path = output_dir.join(format!("{artifact_id}.png"));
    let mut params = ScreenshotParams::builder().full_page(full_page);
    if let Some(clip) = clip {
        params = params.clip(clip).capture_beyond_viewport(true);
    }
    let params = params.build();
    let bytes = page.screenshot(params).await?;
    let byte_count = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if byte_count > MAX_IMAGE_ARTIFACT_BYTES {
        return Err(BrowserError::ImageArtifactTooLarge {
            bytes: byte_count,
            maximum: MAX_IMAGE_ARTIFACT_BYTES,
        });
    }
    let (width, height) = image::ImageReader::new(Cursor::new(&bytes))
        .with_guessed_format()?
        .into_dimensions()?;
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    if pixels > MAX_IMAGE_ARTIFACT_PIXELS {
        return Err(BrowserError::ImageArtifactPixels {
            pixels,
            maximum: MAX_IMAGE_ARTIFACT_PIXELS,
        });
    }
    tokio::fs::write(&path, &bytes).await?;
    Ok(BrowserImageArtifact {
        artifact_id,
        path,
        mime_type: "image/png".to_owned(),
        width,
        height,
        model_image: None,
    })
}

pub(super) fn compare(
    baseline: &BrowserImageArtifact,
    current: &BrowserImageArtifact,
    output_dir: &Path,
    threshold: Option<u8>,
) -> Result<BrowserVisualDiff, BrowserError> {
    let baseline_image = image::open(&baseline.path)?.to_rgba8();
    let current_image = image::open(&current.path)?.to_rgba8();
    let dimensions_match = baseline_image.dimensions() == current_image.dimensions();
    let width = baseline_image.width().max(current_image.width());
    let height = baseline_image.height().max(current_image.height());
    let mut diff = ImageBuffer::from_pixel(width, height, Rgba([0, 0, 0, 255]));
    let threshold = threshold.unwrap_or(DEFAULT_PIXEL_THRESHOLD);
    let mut changed = 0_u64;
    let mut total_delta = 0_u64;
    let mut maximum_delta = 0_u8;
    let total = u64::from(width).saturating_mul(u64::from(height)).max(1);

    for y in 0..height {
        for x in 0..width {
            let left = pixel_or_transparent(&baseline_image, x, y);
            let right = pixel_or_transparent(&current_image, x, y);
            let deltas = [
                left[0].abs_diff(right[0]),
                left[1].abs_diff(right[1]),
                left[2].abs_diff(right[2]),
                left[3].abs_diff(right[3]),
            ];
            let pixel_max = deltas.into_iter().max().unwrap_or_default();
            maximum_delta = maximum_delta.max(pixel_max);
            total_delta = total_delta.saturating_add(
                deltas
                    .into_iter()
                    .map(u64::from)
                    .fold(0_u64, u64::saturating_add),
            );
            if pixel_max > threshold {
                changed = changed.saturating_add(1);
                diff.put_pixel(x, y, Rgba([255, 0, 255, 255]));
            } else {
                let gray = bounded_u8(luminance(left));
                diff.put_pixel(x, y, Rgba([gray, gray, gray, 255]));
            }
        }
    }

    let diff_path = output_dir.join(format!(
        "diff-{}-{}.png",
        baseline.artifact_id, current.artifact_id
    ));
    diff.save(&diff_path)?;
    Ok(BrowserVisualDiff {
        baseline_id: baseline.artifact_id.clone(),
        current_id: current.artifact_id.clone(),
        changed_pixel_ratio: ratio(changed, total),
        mean_channel_delta: ratio(total_delta, total.saturating_mul(4)),
        maximum_channel_delta: maximum_delta,
        dimensions_match,
        diff_path,
        model_image: None,
    })
}

pub(super) fn start_trace(
    page: Page,
    output_dir: PathBuf,
    trace_id: u64,
    frames_per_second: u8,
    max_frames: u16,
) -> VisualTraceState {
    let started = Instant::now();
    let frames = Arc::new(StdMutex::new(Vec::with_capacity(usize::from(max_frames))));
    let dropped = Arc::new(AtomicU64::new(0));
    let task_frames = Arc::clone(&frames);
    let task_dropped = Arc::clone(&dropped);
    let task = tokio::spawn(async move {
        let period = Duration::from_secs_f64(1.0 / f64::from(frames_per_second));
        let mut interval = tokio::time::interval(period);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        for index in 0..usize::from(max_frames) {
            interval.tick().await;
            let params = ScreenshotParams::builder().full_page(false).build();
            let bytes = match page.screenshot(params).await {
                Ok(bytes) => bytes,
                Err(error) => {
                    task_dropped.fetch_add(1, Ordering::Relaxed);
                    warn!(
                        target: "nanocodex_browser",
                        %error,
                        "visual trace frame capture failed"
                    );
                    continue;
                }
            };
            let path = output_dir.join(format!("visual-trace-{trace_id}-{index:04}.png"));
            if let Err(error) = tokio::fs::write(&path, bytes).await {
                task_dropped.fetch_add(1, Ordering::Relaxed);
                warn!(
                    target: "nanocodex_browser",
                    %error,
                    "visual trace frame write failed"
                );
                continue;
            }
            let frame = TraceFrame {
                elapsed: started.elapsed(),
                path,
            };
            if let Ok(mut frames) = task_frames.lock() {
                frames.push(frame);
            } else {
                task_dropped.fetch_add(1, Ordering::Relaxed);
                break;
            }
        }
    });
    VisualTraceState {
        started,
        frames,
        dropped,
        task: Some(task),
    }
}

pub(super) async fn stop_trace(
    trace: VisualTraceState,
    cumulative_layout_shift: f64,
) -> Result<BrowserVisualTrace, BrowserError> {
    let mut trace = trace;
    if let Some(task) = trace.task.take() {
        task.abort();
        let _ = task.await;
    }
    let duration = trace.started.elapsed();
    let frames = trace
        .frames
        .lock()
        .map_err(|_| BrowserError::DiagnosticsUnavailable)?
        .clone();
    let analyzed = frames
        .into_iter()
        .map(analyze_frame)
        .collect::<Result<Vec<_>, _>>()?;
    let mut anomalies = Vec::new();
    let mut maximum_changed_pixel_ratio = 0.0_f64;
    let mut deltas = Vec::with_capacity(analyzed.len().saturating_sub(1));
    for pair in analyzed.windows(2) {
        let ratio = changed_pixel_ratio(&pair[0].image, &pair[1].image, DEFAULT_PIXEL_THRESHOLD);
        maximum_changed_pixel_ratio = maximum_changed_pixel_ratio.max(ratio);
        deltas.push(ratio);
    }

    for index in 0..analyzed.len() {
        let frame = &analyzed[index];
        let previous_change = index
            .checked_sub(1)
            .and_then(|previous| deltas.get(previous))
            .copied()
            .unwrap_or_default();
        let next_change = deltas.get(index).copied().unwrap_or_default();
        let returns_to_previous = index > 0
            && index + 1 < analyzed.len()
            && changed_pixel_ratio(
                &analyzed[index - 1].image,
                &analyzed[index + 1].image,
                DEFAULT_PIXEL_THRESHOLD,
            ) < RETURN_CHANGE_RATIO;
        let kind = if frame.blank_pixel_ratio >= BLANK_PIXEL_RATIO
            && index > 0
            && index + 1 < analyzed.len()
            && analyzed[index - 1].blank_pixel_ratio < BLANK_PIXEL_RATIO
            && analyzed[index + 1].blank_pixel_ratio < BLANK_PIXEL_RATIO
        {
            Some(BrowserVisualAnomalyKind::BlankFrame)
        } else if previous_change >= FLASH_CHANGE_RATIO
            && next_change >= FLASH_CHANGE_RATIO
            && returns_to_previous
        {
            Some(BrowserVisualAnomalyKind::Flash)
        } else if previous_change >= 0.75 {
            Some(BrowserVisualAnomalyKind::LargeVisualChange)
        } else {
            None
        };
        if let Some(kind) = kind {
            let previous_luminance = index
                .checked_sub(1)
                .and_then(|previous| analyzed.get(previous))
                .map_or(frame.luminance, |previous| previous.luminance);
            anomalies.push(BrowserVisualAnomaly {
                kind,
                frame_index: index,
                elapsed_ms: duration_millis(frame.elapsed),
                changed_pixel_ratio: previous_change,
                luminance_delta: (frame.luminance - previous_luminance).abs(),
                frame_path: frame.path.clone(),
                model_image: None,
            });
        }
    }

    Ok(BrowserVisualTrace {
        frame_count: analyzed.len(),
        duration_ms: duration_millis(duration),
        dropped_frames: trace.dropped.load(Ordering::Relaxed),
        maximum_changed_pixel_ratio,
        cumulative_layout_shift,
        anomalies,
    })
}

fn analyze_frame(frame: TraceFrame) -> Result<FrameAnalysis, BrowserError> {
    let image = image::open(&frame.path)?;
    let rgba = image.to_rgba8();
    let pixel_count = u64::from(rgba.width())
        .saturating_mul(u64::from(rgba.height()))
        .max(1);
    let mut luminance_sum = 0.0_f64;
    let mut histogram = [0_u64; 16];
    for pixel in rgba.pixels() {
        let value = luminance(*pixel);
        luminance_sum += value;
        let bucket = usize::from(bounded_u8(value) / 16).min(histogram.len() - 1);
        histogram[bucket] = histogram[bucket].saturating_add(1);
    }
    let dominant = histogram.into_iter().max().unwrap_or_default();
    Ok(FrameAnalysis {
        path: frame.path,
        elapsed: frame.elapsed,
        image: DynamicImage::ImageRgba8(rgba),
        luminance: luminance_sum / u64_as_f64(pixel_count),
        blank_pixel_ratio: ratio(dominant, pixel_count),
    })
}

fn changed_pixel_ratio(left: &DynamicImage, right: &DynamicImage, threshold: u8) -> f64 {
    let left = left.to_rgba8();
    let right = right.to_rgba8();
    let width = left.width().max(right.width());
    let height = left.height().max(right.height());
    let total = u64::from(width).saturating_mul(u64::from(height)).max(1);
    let mut changed = 0_u64;
    for y in 0..height {
        for x in 0..width {
            let left = pixel_or_transparent(&left, x, y);
            let right = pixel_or_transparent(&right, x, y);
            if (0..4).any(|channel| left[channel].abs_diff(right[channel]) > threshold) {
                changed = changed.saturating_add(1);
            }
        }
    }
    ratio(changed, total)
}

fn pixel_or_transparent(image: &ImageBuffer<Rgba<u8>, Vec<u8>>, x: u32, y: u32) -> Rgba<u8> {
    if x < image.width() && y < image.height() {
        *image.get_pixel(x, y)
    } else {
        Rgba([0, 0, 0, 0])
    }
}

fn luminance(pixel: Rgba<u8>) -> f64 {
    0.2126 * f64::from(pixel[0]) + 0.7152 * f64::from(pixel[1]) + 0.0722 * f64::from(pixel[2])
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the value is rounded and clamped to the complete u8 range first"
)]
const fn bounded_u8(value: f64) -> u8 {
    value.round().clamp(0.0, 255.0) as u8
}

#[allow(
    clippy::cast_precision_loss,
    reason = "image dimensions bound these counters and ratios intentionally return f64"
)]
const fn u64_as_f64(value: u64) -> f64 {
    value as f64
}

fn ratio(numerator: u64, denominator: u64) -> f64 {
    u64_as_f64(numerator) / u64_as_f64(denominator.max(1))
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}
