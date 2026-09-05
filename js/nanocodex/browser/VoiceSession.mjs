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
  #call;
  #flushTimer;
  #reconnectTimer;
  #inbound = Promise.resolve();
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
      if (this.#closed) stopStream(microphone);
      else this.#microphone = microphone;
      return microphone;
    });
    const core = await this.#options.core;
    this.#core = core;
    if (this.#closed) {
      await microphoneCapture.catch(() => {});
      core.free();
      this.#core = undefined;
      return;
    }
    await this.#options.beforeAgentTurn?.();
    let microphone;
    try {
      [, microphone] = await Promise.all([core.start(), microphoneCapture]);
    } catch (cause) {
      if (this.#closed) return;
      throw cause;
    }
    if (this.#closed) {
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
      const stream = event.streams[0] ?? new MediaStream([event.track]);
      this.#speaker ??= new SpeakerPlayback(new Audio(), this.#options.onStatus);
      this.#speaker.attach(stream);
    });
    peer.addEventListener("connectionstatechange", () => {
      if (peer.connectionState === "failed" || peer.connectionState === "disconnected") {
        this.#options.onTerminated(`Voice ${peer.connectionState} — tap Voice to reconnect`);
      }
    });

    const offer = await peer.createOffer();
    await peer.setLocalDescription(offer);
    try {
      await waitForIce(peer, this.#closing.signal);
    } catch (cause) {
      if (this.#closed) return;
      this.#stopBrowserIo();
      throw cause;
    }
    if (this.#closed || peer.signalingState === "closed") return;
    const sdp = peer.localDescription?.sdp;
    if (!sdp) throw new Error("the browser did not produce a Realtime WebRTC offer");

    const call = new AbortController();
    this.#call = call;
    const body = await core.callBody(sdp);
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
    await peer.setRemoteDescription({ type: "answer", sdp: completed.sdp });
    if (this.#closed) return;

    this.#sidebandCallId = completed.call_id;
    this.#sidebandUrl = this.#options.sidebandUrl
      ? undefined
      : String(await core.sidebandUrl(completed.call_id));
    try {
      await this.#openSideband();
    } catch (cause) {
      if (this.#closed) return;
      this.#stopBrowserIo();
      throw cause;
    }
    if (this.#closed) return;
    this.#status(`Voice active (${this.#options.voice}) — /voice off to stop`);
  }

  observe(envelope) {
    if (!this.#closed && this.#core) {
      this.#enqueue(() => this.#core.agentEvent(JSON.stringify(envelope)));
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
    if (this.#closed && this.#closePromise) return;
    this.#closed = true;
    this.#closing.abort();
    this.#stopBrowserIo();
    this.#core?.free();
    this.#core = undefined;
    this.#closePromise = Promise.resolve();
  }

  async #finishClose() {
    try {
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

  #enqueue(operation) {
    if (this.#closed) return Promise.resolve();
    const next = this.#inbound.then(operation).then((effects) => this.#apply(effects));
    this.#inbound = next.catch((error) => {
      if (!this.#closed) this.#options.onTerminated(errorMessage(error));
    });
    return next;
  }

  async #apply(encoded) {
    const effects = typeof encoded === "string" ? JSON.parse(encoded) : encoded;
    if (!effects || typeof effects !== "object") return;
    let sent = 0;
    for (const frame of effects.frames ?? []) {
      if (this.#sideband?.readyState === WebSocket.OPEN) {
        this.#sideband.send(frame);
        sent += 1;
      }
    }
    if (effects.acknowledge_frames && sent > 0) await this.#core?.framesSent(sent);
    for (const entry of effects.transcripts ?? []) {
      this.#options.onTranscript(entry.speaker, entry.text);
    }
    if (effects.status) this.#status(effects.status);
    if (effects.schedule_flush && this.#flushTimer === undefined && !this.#closed) {
      this.#flushTimer = window.setTimeout(() => {
        this.#flushTimer = undefined;
        if (this.#core && !this.#closed) this.#enqueue(() => this.#core.flush(false));
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
        this.#enqueue(async () => {
          if (await this.#core.requiresAgentAdmission(event.data)) {
            await this.#options.beforeAgentTurn?.();
          }
          return this.#core.realtimeMessage(event.data);
        });
      }
    });
    sideband.addEventListener("close", () => {
      if (!opened || this.#closed || generation !== this.#sidebandGeneration) return;
      const connectedMs = Math.max(0, Date.now() - this.#sidebandOpenedAt);
      this.#enqueue(() => this.#core.sidebandClosed(Math.min(connectedMs, 0xffff_ffff)));
    });
    await waitForWebSocket(sideband, this.#closing.signal);
    if (this.#closed || generation !== this.#sidebandGeneration) {
      sideband.close();
      return;
    }
    opened = true;
    this.#sidebandOpenedAt = Date.now();
    await this.#enqueue(() => this.#core.sidebandOpened());
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

function waitForIce(peer, signal) {
  if (peer.iceGatheringState === "complete") return Promise.resolve();
  return new Promise((resolve, reject) => {
    let timer;
    const changed = () => {
      if (peer.iceGatheringState !== "complete") return;
      cleanup();
      resolve();
    };
    const stopped = () => {
      cleanup();
      reject(new Error("voice connection stopped"));
    };
    const timedOut = () => {
      cleanup();
      peer.close();
      reject(new VoiceError(
        "ice_gathering_timeout",
        "Realtime voice network negotiation did not finish in time. Check your network connection, then retry.",
      ));
    };
    const cleanup = () => {
      window.clearTimeout(timer);
      peer.removeEventListener("icegatheringstatechange", changed);
      signal?.removeEventListener("abort", stopped);
    };
    timer = window.setTimeout(timedOut, ICE_GATHERING_TIMEOUT_MS);
    peer.addEventListener("icegatheringstatechange", changed);
    signal?.addEventListener("abort", stopped, { once: true });
    if (signal?.aborted) stopped();
    else changed();
  });
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
