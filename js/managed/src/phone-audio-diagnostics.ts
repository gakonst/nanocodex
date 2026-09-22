/** Bounded numeric call-quality metadata. Audio and arbitrary provider fields never cross this boundary. */
const COUNTERS = new Set([
  "inbound_frames", "inbound_bytes", "sequence_gaps", "duplicate_events", "timestamp_overlaps",
  "outbound_frames", "outbound_bytes", "clear_events", "cleared_marks", "acknowledged_marks",
  "unmatched_marks", "input_backpressure", "pending_marks", "input_samples", "clipped_samples", "silent_frames",
]);
const DURATIONS = new Set(["timestamp_gap_ms", "peak_pending_audio_ms", "pending_audio_ms"]);
const LEVELS = new Set(["input_rms_dbfs", "input_peak_dbfs"]);

export function validPhoneAudioDiagnostics(value: unknown): value is Record<string, number> {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const entries = Object.entries(value);
  if (!entries.length) return false;
  return entries.every(([key, number]) => {
    if (typeof number !== "number" || !Number.isFinite(number)) return false;
    if (key === "version") return number === 1;
    if (COUNTERS.has(key)) return Number.isSafeInteger(number) && number >= 0;
    if (DURATIONS.has(key)) return number >= 0 && number <= Number.MAX_SAFE_INTEGER;
    if (LEVELS.has(key)) return number >= -120 && number <= 0;
    return false;
  });
}
