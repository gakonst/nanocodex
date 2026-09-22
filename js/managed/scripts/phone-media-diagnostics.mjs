// Numeric transport and aggregate level measurements only: never retain audio, transcript, or provider IDs.
const integer = value => typeof value === 'string' && /^\d+$/.test(value) && Number.isSafeInteger(Number(value)) ? Number(value) : undefined;
// G.711 mu-law inverse companding. The largest code decodes to 32124,
// so count saturation at that codec ceiling rather than unreachable PCM 32767.
const muLawPcm = Uint16Array.from({ length: 256 }, (_, code) => {
  const value = (~code) & 255;
  return (((value & 15) << 3) + 132) * (2 ** ((value >> 4) & 7)) - 132;
});
const dbfs = amplitude => amplitude > 0 ? Math.max(-120, 20 * Math.log10(amplitude / 32768)) : -120;
export function createMediaDiagnostics() {
  const counters = { inbound_frames: 0, inbound_bytes: 0, sequence_gaps: 0, duplicate_events: 0,
    timestamp_gap_ms: 0, timestamp_overlaps: 0, outbound_frames: 0, outbound_bytes: 0,
    clear_events: 0, cleared_marks: 0, acknowledged_marks: 0, unmatched_marks: 0,
    peak_pending_audio_ms: 0, input_backpressure: 0, input_samples: 0, clipped_samples: 0, silent_frames: 0 };
  let sequence, endTimestamp, pendingBytes = 0, inputSumSquares = 0, inputPeak = 0;
  const marks = new Map();
  return {
    sequence(value) {
      const next = integer(value);
      // Legacy peers without metadata still work; metadata never changes wire contracts.
      if (next === undefined) return true;
      if (sequence !== undefined && next <= sequence) { counters.duplicate_events++; return false; }
      if (sequence !== undefined) counters.sequence_gaps += next - sequence - 1;
      sequence = next; return true;
    },
    input(media, audio) {
      const bytes = typeof audio === 'number' ? audio : audio.length;
      if (typeof audio !== 'number') {
        let framePeak = 0;
        for (const code of audio) {
          const amplitude = muLawPcm[code];
          inputSumSquares += amplitude * amplitude;
          framePeak = Math.max(framePeak, amplitude);
          if (amplitude === 32124) counters.clipped_samples++;
        }
        counters.input_samples += bytes;
        inputPeak = Math.max(inputPeak, framePeak);
        // Below approximately -60 dBFS across the entire frame.
        if (bytes > 0 && framePeak <= 32) counters.silent_frames++;
      }
      const timestamp = integer(media.timestamp);
      if (timestamp !== undefined) {
        if (endTimestamp !== undefined) {
          if (timestamp > endTimestamp) counters.timestamp_gap_ms += timestamp - endTimestamp;
          if (timestamp < endTimestamp) counters.timestamp_overlaps++;
        }
        endTimestamp = timestamp + bytes / 8; // mono 8kHz mu-law: eight bytes per ms
      }
      counters.inbound_frames++; counters.inbound_bytes += bytes; return true;
    },
    // Bound both tiny-frame overhead and audio time (the former 100-frame bound
    // alone permitted 100 seconds when the native peer emitted 1-second chunks).
    canQueue(bytes) { return marks.size < 100 && pendingBytes + bytes <= 80_000; },
    queue(name, bytes) {
      marks.set(name, bytes); pendingBytes += bytes;
      counters.outbound_frames++; counters.outbound_bytes += bytes;
      counters.peak_pending_audio_ms = Math.max(counters.peak_pending_audio_ms, pendingBytes / 8);
    },
    mark(name) {
      if (!marks.has(name)) { counters.unmatched_marks++; return; }
      pendingBytes -= marks.get(name); marks.delete(name); counters.acknowledged_marks++;
    },
    clear() {
      counters.clear_events++; counters.cleared_marks += marks.size;
      marks.clear(); pendingBytes = 0;
      // Twilio also returns old marks after clear; monotonically unique names
      // prevent those acknowledgements from releasing new playback.
    },
    inputBackpressure() { counters.input_backpressure++; },
    snapshot() { return { version: 1, ...counters, input_rms_dbfs: dbfs(counters.input_samples ? Math.sqrt(inputSumSquares / counters.input_samples) : 0), input_peak_dbfs: dbfs(inputPeak), pending_audio_ms: pendingBytes / 8, pending_marks: marks.size }; },
  };
}
