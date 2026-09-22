/** Plays account-proxied PCM without exposing provider credentials to the browser. */
export class ElevenLabsPlayback {
  #synthesize;
  #onError;
  #context;
  #abort = new AbortController();
  #queue = Promise.resolve();
  #sources = new Set();
  #nextTime = 0;
  #caption;
  #offset = 0;
  #suppressedThrough = -1;
  #pending = 0;
  #pendingChars = 0;
  #closed = false;

  constructor(synthesize, onError) {
    this.#synthesize = synthesize;
    this.#onError = onError;
  }

  prime() {
    if (this.#closed) return;
    this.#context ??= new AudioContext({ sampleRate: 24000 });
    return this.#context.resume();
  }

  transcript(entry) {
    if (this.#closed || entry.speaker !== "assistant" || entry.id <= this.#suppressedThrough) return;
    if (this.#caption !== undefined && entry.id < this.#caption) return;
    if (entry.id !== this.#caption) { this.#caption = entry.id; this.#offset = 0; }
    const remaining = entry.text.slice(this.#offset);
    // Buffer partial words; emit complete sentences before the turn finishes.
    const boundary = entry.is_partial ? [...remaining.matchAll(/[.!?\n](?:\s|$)/g)].at(-1) : undefined;
    const length = entry.is_partial ? (boundary ? boundary.index + boundary[0].length : 0) : remaining.length;
    const text = remaining.slice(0, length).trim();
    this.#offset += length;
    if (!text) return;
    if (this.#pending >= 32 || this.#pendingChars + text.length > 32768) {
      this.interrupt();
      this.#onError(new Error("ElevenLabs playback queue exceeded its limit"));
      return;
    }
    this.#pending++;
    this.#pendingChars += text.length;
    const signal = this.#abort.signal;
    this.#queue = this.#queue.then(async () => {
      if (signal.aborted) return;
      this.#context ??= new AudioContext({ sampleRate: 24000 });
      await this.#context.resume();
      if (signal.aborted) return;
      const response = await this.#synthesize(text, signal);
      if (!response.ok) throw new Error(`ElevenLabs synthesis failed (${response.status})`);
      const reader = response.body?.getReader();
      if (!reader) throw new Error("ElevenLabs returned no audio stream");
      const cancel = () => { void reader.cancel().catch(() => {}); };
      signal.addEventListener("abort", cancel, { once: true });
      let carry;
      try {
        while (!signal.aborted) {
          const { value, done } = await reader.read();
          if (done) break;
          if (signal.aborted) break;
          if (value.byteLength > 1024 * 1024) throw new Error("ElevenLabs audio chunk exceeded its limit");
          const bytes = carry === undefined ? value : Uint8Array.from([carry, ...value]);
          const size = bytes.length - bytes.length % 2;
          carry = size < bytes.length ? bytes[size] : undefined;
          if (!size) continue;
          // Keep decoded/scheduled audio bounded even when synthesis outruns playback.
          for (let offset = 0; offset < size && !signal.aborted; offset += 9600) {
            while (!signal.aborted && (this.#nextTime - this.#context.currentTime > 2 || this.#sources.size >= 128)) {
              await new Promise((resolve) => {
                const done = () => { clearTimeout(timer); signal.removeEventListener("abort", done); resolve(); };
                const timer = setTimeout(done, 25);
                signal.addEventListener("abort", done, { once: true });
              });
            }
            if (signal.aborted) break;
            const length = Math.min(9600, size - offset);
            const buffer = this.#context.createBuffer(1, length / 2, 24000);
            const output = buffer.getChannelData(0);
            const view = new DataView(bytes.buffer, bytes.byteOffset + offset, length);
            for (let i = 0; i < output.length; i++) output[i] = view.getInt16(i * 2, true) / 32768;
            const source = this.#context.createBufferSource();
            source.buffer = buffer;
            source.connect(this.#context.destination);
            this.#sources.add(source);
            source.onended = () => { this.#sources.delete(source); source.disconnect(); };
            this.#nextTime = Math.max(this.#nextTime, this.#context.currentTime + 0.02);
            source.start(this.#nextTime);
            this.#nextTime += buffer.duration;
          }
        }
      } finally { signal.removeEventListener("abort", cancel); await reader.cancel().catch(() => {}); reader.releaseLock(); }
    }).catch((error) => { if (!signal.aborted) this.#onError(error); }).finally(() => {
      if (!signal.aborted) { this.#pending--; this.#pendingChars -= text.length; }
    });
  }

  interrupt() {
    this.#suppressedThrough = Math.max(this.#suppressedThrough, this.#caption ?? -1);
    this.#pending = 0;
    this.#pendingChars = 0;
    this.#abort.abort();
    this.#abort = new AbortController();
    this.#queue = Promise.resolve();
    for (const source of this.#sources) { try { source.stop(); } catch {} source.disconnect(); }
    this.#sources.clear();
    this.#nextTime = 0;
    // Caption IDs are monotonic: every late delta/final for this epoch stays silent.
  }

  close() {
    this.#closed = true;
    this.interrupt();
    void this.#context?.close();
    this.#context = undefined;
  }
}
