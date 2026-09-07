export const MICROPHONE_CAPTURE_TIMEOUT_MS = 15_000;
export const ICE_GATHERING_TIMEOUT_MS = 15_000;
export const REALTIME_CALL_TIMEOUT_MS = 15_000;
export const SIDEBAND_OPEN_TIMEOUT_MS = 15_000;

export class VoiceError extends Error {
  constructor(code, message, options = {}) {
    super(message, options.cause === undefined ? undefined : { cause: options.cause });
    this.name = "VoiceError";
    this.code = code;
  }
}

/** Owns browser speaker playback and retries it from the next user gesture when autoplay is blocked. */
export class SpeakerPlayback {
  #speaker;
  #gestures;
  #onStatus;
  #resume;
  #closed = false;

  constructor(speaker, onStatus, gestures = document) {
    this.#speaker = speaker;
    this.#onStatus = onStatus;
    this.#gestures = gestures;
    this.#speaker.autoplay = true;
  }

  attach(stream) {
    if (this.#closed) return;
    this.#speaker.srcObject = stream;
    this.#play();
  }

  setEnabled(enabled) {
    this.#speaker.muted = !enabled;
  }

  close() {
    if (this.#closed) return;
    this.#closed = true;
    this.#disarm();
    this.#speaker.pause();
    this.#speaker.srcObject = null;
  }

  #play() {
    if (this.#closed) return;
    this.#disarm();
    void this.#speaker.play().catch(() => {
      if (this.#closed) return;
      this.#onStatus("Voice connected — tap once to enable speaker audio");
      const resume = () => {
        if (this.#resume !== resume) return;
        this.#resume = undefined;
        this.#gestures.removeEventListener("click", resume, true);
        this.#play();
      };
      this.#resume = resume;
      this.#gestures.addEventListener("click", resume, { capture: true, once: true });
    });
  }

  #disarm() {
    if (!this.#resume) return;
    this.#gestures.removeEventListener("click", this.#resume, true);
    this.#resume = undefined;
  }
}

/** Executes browser-only media and network effects for the Rust-owned voice controller. */
export class BrowserVoiceSession {
  #options;
  #core;
  #peer;
  #channel;
  #sideband;
  #sidebandUrl;
  #sidebandCallId;
  #sidebandOpenedAt;
  #sidebandGeneration = 0;
  #microphone;
  #speaker;
  #playbackEnabled = false;
  #call;
  #flushTimer;
  #reconnectTimer;
  #inbound = Promise.resolve();
  #liveUpdates = new Set();
  #starting;
  #closePromise;
  #closed = false;
  #closing = new AbortController();

  constructor(options) {
    this.#options = options;
  }

  start() {
    this.#starting ??= this.#start();
    return this.#starting;
  }

  async #start() {
    if (!this.#options.captureMicrophone && !navigator.mediaDevices?.getUserMedia) {
      throw new Error("this browser does not expose microphone capture");
    }

    // This call intentionally precedes every await so mobile user activation is retained.
    const capture = this.#options.captureMicrophone?.() ?? capturePreferredMicrophone(
      async (current, labels) => {
        const core = await this.#options.core;
        return core.preferredPhysicalInput(current, JSON.stringify(labels));
      },
    );
    const microphoneCapture = acquireMicrophone(capture, this.#closing.signal).then((microphone) => {
      if (this.#closed || this.#closing.signal.aborted) stopStream(microphone);
      else this.#microphone = microphone;
      return microphone;
    });
    // Admission/WASM and browser negotiation are independent until callBody.
    // Observe both immediately so a denied microphone cannot reject unhandled
    // while a slow controller or agent admission is still pending.
    const coreStartup = Promise.resolve(this.#options.core).then(async (core) => {
      if (this.#closed || this.#closing.signal.aborted) { core.free(); return; }
      this.#core = core;
      await this.#options.beforeAgentTurn?.();
      if (this.#closed || this.#closing.signal.aborted) return;
      await core.start();
      return core;
    });
    let core, media;
    try {
      [core, media] = await Promise.all([coreStartup, this.#prepareMedia(microphoneCapture)]);
    } catch (cause) {
      this.#closing.abort();
      this.#stopBrowserIo();
      // A late admission must settle before stop/free can touch its controller.
      await coreStartup.catch(() => {});
      if (this.#closed) return;
      throw cause;
    }
    if (this.#closed || !core || !media) return;
    const { peer, sdp } = media;

    const call = new AbortController();
    this.#call = call;
    const body = await core.callBody(sdp);
    if (this.#closed) return;
    let callResponse;
    try {
      callResponse = await withStartupDeadline(async () => {
        const response = this.#options.call
          ? await this.#options.call(body, call.signal)
          : await fetch(this.#options.callUrl ?? "/api/realtime/calls", {
              method: "POST",
              signal: call.signal,
              credentials: "same-origin",
              headers: {
                "content-type": "application/json",
                "x-nanocodex-request": "1",
              },
              body,
            });
        if (!response.ok) throw new Error(await responseError(response, "voice connection failed"));
        const location = response.headers.get("x-nanocodex-realtime-location");
        if (!location) throw new Error("voice connection did not return a Realtime Location");
        return { location, body: await response.text() };
      }, {
        signal: call.signal,
        timeoutMs: REALTIME_CALL_TIMEOUT_MS,
        onTimeout: () => call.abort(),
        timeoutError: new VoiceError(
          "realtime_call_timeout",
          "The Realtime voice connection request did not finish in time. Check your network connection, then retry.",
        ),
      });
    } catch (cause) {
      if (this.#closed) return;
      this.#stopBrowserIo();
      throw cause;
    }
    const completed = JSON.parse(await core.completeCall(callResponse.body, callResponse.location));
    if (this.#closed || peer.signalingState === "closed") return;
    this.#sidebandCallId = completed.call_id;
    this.#sidebandUrl = this.#options.sidebandUrl
      ? undefined
      : String(await core.sidebandUrl(completed.call_id));
    if (this.#closed) return;
    try {
      await Promise.all([
        peer.setRemoteDescription({ type: "answer", sdp: completed.sdp }),
        this.#openSideband(),
      ]);
    } catch (cause) {
      if (this.#closed) return;
      this.#stopBrowserIo();
      throw cause;
    }
    if (this.#closed) return;
    this.#status(`Voice active (${this.#options.voice}) — /voice off to stop`);
  }

  async #prepareMedia(capture) {
    const microphone = await capture;
    if (this.#closed || this.#closing.signal.aborted) {
      stopStream(microphone);
      return;
    }
    for (const track of microphone.getAudioTracks()) {
      track.contentHint = "speech";
      track.addEventListener("mute", () => this.#status("Voice paused — microphone interrupted"));
      track.addEventListener("unmute", () => this.#status(`Voice active (${this.#options.voice})`));
      track.addEventListener("ended", () => {
        this.#options.onTerminated("Voice microphone ended — tap Voice to reconnect");
      });
    }

    const peer = new RTCPeerConnection();
    this.#peer = peer;
    for (const track of microphone.getAudioTracks()) peer.addTrack(track, microphone);
    this.#channel = peer.createDataChannel("oai-events");
    peer.addEventListener("track", (event) => {
      if (this.#closed || this.#closing.signal.aborted || this.#peer !== peer) {
        event.track.stop();
        return;
      }
      const stream = event.streams[0] ?? new MediaStream([event.track]);
      this.#speaker ??= new SpeakerPlayback(new Audio(), this.#options.onStatus);
      this.#speaker.setEnabled(this.#playbackEnabled);
      this.#speaker.attach(stream);
    });
    peer.addEventListener("connectionstatechange", () => {
      if (this.#closed || this.#closing.signal.aborted || this.#peer !== peer) return;
      if (peer.connectionState === "failed" || peer.connectionState === "disconnected") {
        this.#options.onTerminated(`Voice ${peer.connectionState} — tap Voice to reconnect`);
      }
    });

    const offer = await peer.createOffer();
    if (this.#closed || this.#closing.signal.aborted) return;
    await peer.setLocalDescription(offer);
    if (this.#closed || peer.signalingState === "closed") return;
    // The server supplies candidates in its answer; gather local candidates
    // while that request is in flight instead of waiting for every interface.
    const sdp = offer.sdp;
    if (!sdp) throw new Error("the browser did not produce a Realtime WebRTC offer");

    return { peer, sdp };
  }

  observe(envelope) {
    if (!this.#closed && this.#core) {
      this.#applyLive(() => this.#core.agentEvent(JSON.stringify(envelope)));
    }
  }

  cancel() {
    return this.#core?.cancel() ?? Promise.resolve(false);
  }

  close() {
    if (this.#closePromise) return this.#closePromise;
    this.#closed = true;
    this.#closing.abort();
    // Microphone and speaker ownership ends synchronously. Protocol tail/lifecycle
    // cleanup may legitimately wait behind an independent coding turn.
    this.#stopBrowserMedia();
    this.#closePromise = this.#finishClose();
    return this.#closePromise;
  }

  abort() {
    if (this.#closed && this.#closePromise) return this.#closePromise.catch(() => {});
    this.#closed = true;
    this.#closing.abort();
    this.#stopBrowserIo();
    this.#closePromise = (async () => {
      await this.#starting?.catch(() => {});
      await Promise.all(this.#liveUpdates);
      await this.#inbound;
      this.#core?.free();
      this.#core = undefined;
    })();
    return this.#closePromise;
  }

  async #finishClose() {
    try {
      await this.#starting?.catch(() => {});
      await Promise.all(this.#liveUpdates);
      await this.#inbound;
      if (this.#core) {
        await this.#options.beforeAgentTurn?.();
        await this.#apply(await this.#core.stop());
      }
    } finally {
      this.#stopBrowserIo();
      this.#core?.free();
      this.#core = undefined;
    }
  }

  #enqueue(operation, accepted = false) {
    if (this.#closed && !accepted) return Promise.resolve();
    const next = this.#inbound.then(operation).then((effects) => this.#apply(effects));
    this.#inbound = next.catch((error) => {
      if (!this.#closed) this.#options.onTerminated(errorMessage(error));
    });
    return next;
  }

  #applyLive(operation) {
    if (this.#closed) return;
    const next = Promise.resolve().then(operation).then((effects) => this.#apply(effects))
      .catch((error) => {
        if (!this.#closed) this.#options.onTerminated(errorMessage(error));
      }).finally(() => this.#liveUpdates.delete(next));
    this.#liveUpdates.add(next);
    return next;
  }

  async #apply(encoded) {
    const effects = typeof encoded === "string" ? JSON.parse(encoded) : encoded;
    if (!effects || typeof effects !== "object") return;
    if (effects.playback_enabled === false) {
      this.#playbackEnabled = false;
      this.#speaker?.setEnabled(false);
    }
    let sent = 0;
    for (const frame of effects.frames ?? []) {
      if (this.#sideband?.readyState === WebSocket.OPEN) {
        this.#sideband.send(frame);
        sent += 1;
      }
    }
    if (effects.acknowledge_frames && sent > 0) await this.#core?.framesSent(sent);
    if (effects.playback_enabled === true && sent === (effects.frames?.length ?? 0)) {
      this.#playbackEnabled = true;
      this.#speaker?.setEnabled(true);
    }
    for (const entry of effects.transcripts ?? []) {
      this.#options.onTranscript(entry.speaker, entry.text, entry);
    }
    if (effects.status) this.#status(effects.status);
    if (effects.schedule_flush && this.#flushTimer === undefined && !this.#closed) {
      this.#flushTimer = window.setTimeout(() => {
        this.#flushTimer = undefined;
        if (this.#core && !this.#closed) this.#applyLive(() => this.#core.flush(false));
      }, 200);
    }
    if (
      effects.reconnect_after_ms !== undefined
      && this.#reconnectTimer === undefined
      && !this.#closed
    ) {
      this.#reconnectTimer = window.setTimeout(() => {
        this.#reconnectTimer = undefined;
        if (this.#closed) return;
        void this.#openSideband().catch((error) => {
          if (!this.#closed) this.#options.onTerminated(errorMessage(error));
        });
      }, effects.reconnect_after_ms);
    }
    if (effects.terminate && !this.#closed) this.#options.onTerminated(effects.terminate);
  }

  async #openSideband() {
    const generation = ++this.#sidebandGeneration;
    const sidebandUrl = this.#options.sidebandUrl
      ? await this.#options.sidebandUrl(this.#sidebandCallId, this.#options.sessionId)
      : this.#sidebandUrl;
    if (this.#closed || generation !== this.#sidebandGeneration) return;
    const sideband = new WebSocket(String(sidebandUrl));
    this.#sideband = sideband;
    let opened = false;
    sideband.addEventListener("message", (event) => {
      if (!this.#closed && generation === this.#sidebandGeneration) {
        this.#applyLive(async () => {
          if (await this.#core.requiresAgentAdmission(event.data)) {
            // Only delegations wait for durable admission. Speech deltas and
            // agent output must continue while that independent request waits.
            void this.#enqueue(async () => {
              await this.#options.beforeAgentTurn?.();
              return this.#core.realtimeMessage(event.data);
            }, true).catch(() => {});
            return;
          }
          return this.#core.realtimeMessage(event.data);
        });
      }
    });
    sideband.addEventListener("close", () => {
      if (!opened || this.#closed || generation !== this.#sidebandGeneration) return;
      const connectedMs = Math.max(0, Date.now() - this.#sidebandOpenedAt);
      this.#applyLive(() => this.#core.sidebandClosed(Math.min(connectedMs, 0xffff_ffff)));
    });
    await waitForWebSocket(sideband, this.#closing.signal);
    if (this.#closed || generation !== this.#sidebandGeneration) {
      sideband.close();
      return;
    }
    opened = true;
    this.#sidebandOpenedAt = Date.now();
    await this.#applyLive(() => this.#core.sidebandOpened());
    if (!this.#closed && generation === this.#sidebandGeneration) {
      this.#status(`Voice active (${this.#options.voice})`);
    }
  }

  #status(message) {
    if (!this.#closed) this.#options.onStatus(message);
  }

  #stopBrowserIo() {
    this.#stopBrowserMedia();
    this.#sidebandGeneration += 1;
    this.#sideband?.close();
    this.#sideband = undefined;
  }

  #stopBrowserMedia() {
    this.#call?.abort();
    this.#call = undefined;
    if (this.#flushTimer !== undefined) window.clearTimeout(this.#flushTimer);
    this.#flushTimer = undefined;
    if (this.#reconnectTimer !== undefined) window.clearTimeout(this.#reconnectTimer);
    this.#reconnectTimer = undefined;
    this.#channel?.close();
    this.#channel = undefined;
    this.#peer?.close();
    this.#peer = undefined;
    stopStream(this.#microphone);
    this.#microphone = undefined;
    this.#speaker?.close();
    this.#speaker = undefined;
  }
}

export async function capturePreferredMicrophone(selectPhysicalInput) {
  let microphone;
  try {
    microphone = await navigator.mediaDevices.getUserMedia({
      audio: {
        autoGainControl: true,
        channelCount: 1,
        echoCancellation: true,
        noiseSuppression: true,
      },
    });
  } catch (cause) {
    throw microphoneCaptureError(cause);
  }
  const current = microphone.getAudioTracks()[0];
  if (!current?.label || !navigator.mediaDevices.enumerateDevices) return microphone;
  try {
    const devices = await navigator.mediaDevices.enumerateDevices();
    const inputs = devices.filter((device) => device.kind === "audioinput" && device.label);
    const index = await selectPhysicalInput(current.label, inputs.map((device) => device.label));
    const physical = index === undefined ? undefined : inputs[index];
    if (physical?.deviceId && physical.deviceId !== current.getSettings?.().deviceId) {
      try {
        const replacement = await navigator.mediaDevices.getUserMedia({ audio: { deviceId: { exact: physical.deviceId } } });
        stopStream(microphone);
        microphone = replacement;
      } catch {
        // Exact-device reselection is only a desktop convenience; retain the usable capture.
      }
    }
  } catch (cause) {
    stopStream(microphone);
    throw cause;
  }
  return microphone;
}

function microphoneCaptureError(cause) {
  if (cause instanceof VoiceError) return cause;
  const name = cause && typeof cause === "object" ? cause.name : undefined;
  if (name === "NotAllowedError" || name === "SecurityError") {
    const policy = document.permissionsPolicy ?? document.featurePolicy;
    const embedded = window.top !== window;
    if (embedded && policy?.allowsFeature?.("microphone") === false) {
      return new VoiceError(
        "microphone_permission_blocked",
        'Microphone access is blocked by this embed. The host iframe must allow="microphone".',
        { cause },
      );
    }
    return new VoiceError(
      "microphone_permission_blocked",
      "Microphone access is blocked for this site. Allow it in your browser settings, then retry.",
      { cause },
    );
  }
  if (name === "NotFoundError" || name === "DevicesNotFoundError") {
    return new VoiceError(
      "microphone_not_found",
      "No microphone was found. Connect a microphone, then retry.",
      { cause },
    );
  }
  if (name === "NotReadableError" || name === "TrackStartError" || name === "AbortError") {
    return new VoiceError(
      "microphone_unavailable",
      "The microphone is unavailable. Close other apps using it, then retry.",
      { cause },
    );
  }
  return cause;
}

function acquireMicrophone(capture, signal) {
  return new Promise((resolve, reject) => {
    let settled = false;
    const finish = (settle, value) => {
      if (settled) return false;
      settled = true;
      window.clearTimeout(timer);
      signal?.removeEventListener("abort", stopped);
      settle(value);
      return true;
    };
    const stopped = () => {
      finish(
        reject,
        new VoiceError("microphone_capture_cancelled", "Microphone capture was stopped."),
      );
    };
    const timer = window.setTimeout(() => {
      finish(
        reject,
        new VoiceError(
          "microphone_capture_timeout",
          "The microphone did not start in time. Check your browser's selected microphone or reconnect it, then retry.",
        ),
      );
    }, MICROPHONE_CAPTURE_TIMEOUT_MS);
    signal?.addEventListener("abort", stopped, { once: true });
    if (signal?.aborted) stopped();
    Promise.resolve(capture).then(
      (microphone) => {
        if (!finish(resolve, microphone)) stopStream(microphone);
      },
      (cause) => { finish(reject, microphoneCaptureError(cause)); },
    );
  });
}

function realtimeSidebandUrl(callId, sessionId) {
  const url = new URL("/api/realtime/sideband", location.href);
  url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
  url.searchParams.set("call_id", callId);
  url.searchParams.set("session_id", sessionId);
  return url;
}

function waitForWebSocket(socket, signal) {
  if (socket.readyState === WebSocket.OPEN) return Promise.resolve();
  return new Promise((resolve, reject) => {
    let timer;
    const opened = () => { cleanup(); resolve(); };
    const failed = () => {
      cleanup();
      socket.close();
      reject(new Error("voice sideband connection failed"));
    };
    const closed = () => { cleanup(); reject(new Error("voice sideband closed before opening")); };
    const stopped = () => {
      cleanup();
      socket.close();
      reject(new Error("voice connection stopped"));
    };
    const timedOut = () => {
      cleanup();
      socket.close();
      reject(new VoiceError(
        "sideband_open_timeout",
        "The Realtime voice sideband did not open in time. Check your network connection, then retry.",
      ));
    };
    const cleanup = () => {
      window.clearTimeout(timer);
      socket.removeEventListener("open", opened);
      socket.removeEventListener("error", failed);
      socket.removeEventListener("close", closed);
      signal?.removeEventListener("abort", stopped);
    };
    timer = window.setTimeout(timedOut, SIDEBAND_OPEN_TIMEOUT_MS);
    socket.addEventListener("open", opened);
    socket.addEventListener("error", failed);
    socket.addEventListener("close", closed);
    signal?.addEventListener("abort", stopped, { once: true });
    if (signal?.aborted) stopped();
    else if (socket.readyState === WebSocket.OPEN) opened();
  });
}

function withStartupDeadline(task, { signal, timeoutMs, onTimeout, timeoutError }) {
  return new Promise((resolve, reject) => {
    let settled = false;
    let timedOut = false;
    let timer;
    const cleanup = () => {
      window.clearTimeout(timer);
      signal?.removeEventListener("abort", stopped);
    };
    const finish = (settle, value) => {
      if (settled) return false;
      settled = true;
      cleanup();
      settle(value);
      return true;
    };
    const stopped = () => {
      if (timedOut) return;
      finish(reject, new Error("voice connection stopped"));
    };
    const timeout = () => {
      if (settled) return;
      timedOut = true;
      cleanup();
      onTimeout?.();
      settled = true;
      reject(timeoutError);
    };
    timer = window.setTimeout(timeout, timeoutMs);
    signal?.addEventListener("abort", stopped, { once: true });
    if (signal?.aborted) {
      stopped();
      return;
    }
    let result;
    try {
      result = task();
    } catch (cause) {
      finish(reject, cause);
      return;
    }
    Promise.resolve(result).then(
      (value) => { finish(resolve, value); },
      (cause) => { finish(reject, cause); },
    );
  });
}

async function responseError(response, fallback) {
  const body = await response.text().catch(() => "");
  try {
    const decoded = JSON.parse(body);
    if (typeof decoded?.error === "string") return decoded.error;
  } catch {}
  return body.trim() || fallback;
}

function stopStream(stream) {
  for (const track of stream?.getTracks?.() ?? []) track.stop();
}

function errorMessage(error) {
  return error instanceof Error ? error.message : String(error);
}
