/** Monotonic offsets from this connection attempt, retained while Stats is
 * closed. These are startup milestones, not network RTT or display latency. */
export type RemoteStartupTiming = Readonly<{
  catalogReadyMs?: number;
  iceReadyMs?: number;
  socketOpenMs?: number;
  offerReceivedMs?: number;
  answerSentMs?: number;
  peerConnectedMs?: number;
  controlsReadyMs?: number;
}>;
type CandidateType = "host" | "srflx" | "prflx" | "relay";
type CandidateProtocol = "udp" | "tcp";
type AddressFamily = "ipv4" | "ipv6" | "unknown";

/** Receiver measurements. Delays are local decode/jitter averages or transport
 * RTT, never an estimate of capture-to-display latency. Missing data stays absent. */
export type RemoteStats = Readonly<{
  decodeFps?: number;
  bitrateKbps?: number;
  roundTripMs?: number;
  decodeMs?: number;
  jitterBufferMs?: number;
  droppedFrames?: number;
  width?: number;
  height?: number;
  codec?: string;
  firstFrameMs?: number;
  totalFirstFrameMs?: number;
  selectionFirstFrameMs?: number;
  preparationMs?: number;
  attempt?: number;
  icePolicy?: "all" | "relay";
  startup?: RemoteStartupTiming;
  localCandidateType?: CandidateType;
  remoteCandidateType?: CandidateType;
  candidateProtocol?: CandidateProtocol;
  relayProtocol?: CandidateProtocol | "tls";
  localAddressFamily?: AddressFamily;
  remoteAddressFamily?: AddressFamily;
}>;

type Stat = { id: string; type: string; timestamp: number; [key: string]: unknown };
const number = (value: unknown): number | undefined => typeof value === "number" && Number.isFinite(value) && value >= 0 ? value : undefined;
function choice<T extends string>(value: unknown, choices: readonly T[]): T | undefined {
  return typeof value === "string" && choices.includes(value as T) ? value as T : undefined;
}
function addressFamily(value: unknown): AddressFamily {
  if (typeof value !== "string" || value.length > 128) return "unknown";
  if (/^(?:[0-9]{1,3}\.){3}[0-9]{1,3}$/.test(value) && value.split(".").every(part => Number(part) <= 255)) return "ipv4";
  if (!value.includes(":") || !/^[0-9a-f:.]+$/i.test(value)) return "unknown"; // Includes browser-redacted mDNS names.
  try {
    // URL validates IPv6 syntax locally. Never retain the address or URL.
    const host = new URL(`http://[${value}]/`).hostname;
    return host.startsWith("[") && host.endsWith("]") ? "ipv6" : "unknown";
  } catch { return "unknown"; }
}
function delta(current: unknown, previous: unknown): number | undefined {
  const a = number(current), b = number(previous);
  return a !== undefined && b !== undefined && a >= b ? a - b : undefined;
}

/** Cumulative counters must be differenced over the report's own timestamp.
 * A new SSRC, counter reset, or cached report cannot produce a rate sample. */
export class RemoteStatsSampler {
  private previous?: Stat;
  sample(report: ReadonlyMap<string, Stat>, trackId?: string): RemoteStats {
    const videos = [...report.values()].filter(value => value.type === "inbound-rtp"
      && (value.kind === "video" || value.mediaType === "video") && value.isRemote !== true);
    const video = (trackId ? videos.find(value => value.trackIdentifier === trackId) : undefined)
      ?? (videos.length === 1 && (!trackId || videos[0]!.trackIdentifier === undefined) ? videos[0] : undefined);
    if (!video) { this.previous = undefined; return {}; }
    const stats: { -readonly [K in keyof RemoteStats]: RemoteStats[K] } = {
      width: number(video.frameWidth), height: number(video.frameHeight),
    };
    const codec = typeof video.codecId === "string" ? report.get(video.codecId) : undefined;
    if (typeof codec?.mimeType === "string") stats.codec = codec.mimeType.replace(/^video\//, "");
    const transport = typeof video.transportId === "string" ? report.get(video.transportId) : undefined;
    let pair = typeof transport?.selectedCandidatePairId === "string" ? report.get(transport.selectedCandidatePairId) : undefined;
    if (!pair && typeof transport?.selectedCandidatePairId !== "string") {
      const candidates = [...report.values()].filter(value => value.type === "candidate-pair" && value.state === "succeeded" && value.nominated === true
        && (typeof video.transportId !== "string" || typeof value.transportId !== "string" || value.transportId === video.transportId));
      if (candidates.length === 1) pair = candidates[0];
    }
    const rtt = number(pair?.currentRoundTripTime);
    if (rtt !== undefined) stats.roundTripMs = rtt * 1000;
    // Copy only bounded enums from the selected media path. Candidate reports
    // also contain addresses, URLs and identifiers that must never reach UI.
    const local = typeof pair?.localCandidateId === "string" ? report.get(pair.localCandidateId) : undefined;
    const remote = typeof pair?.remoteCandidateId === "string" ? report.get(pair.remoteCandidateId) : undefined;
    stats.localCandidateType = choice(local?.candidateType, ["host", "srflx", "prflx", "relay"]);
    stats.remoteCandidateType = choice(remote?.candidateType, ["host", "srflx", "prflx", "relay"]);
    stats.candidateProtocol = choice(local?.protocol, ["udp", "tcp"]);
    if (local) stats.localAddressFamily = addressFamily(local.address ?? local.ip);
    if (remote) stats.remoteAddressFamily = addressFamily(remote.address ?? remote.ip);
    if (stats.localCandidateType === "relay") stats.relayProtocol = choice(local?.relayProtocol, ["udp", "tcp", "tls"]);
    const previous = this.previous;
    this.previous = { ...video };
    if (!previous || previous.id !== video.id) return stats;
    const elapsed = delta(video.timestamp, previous.timestamp);
    if (elapsed === undefined || elapsed === 0) return stats;
    const frames = delta(video.framesDecoded, previous.framesDecoded);
    const bytes = delta(video.bytesReceived, previous.bytesReceived);
    if (frames !== undefined) stats.decodeFps = frames * 1000 / elapsed;
    if (bytes !== undefined) stats.bitrateKbps = bytes * 8 / elapsed;
    const decode = delta(video.totalDecodeTime, previous.totalDecodeTime);
    if (decode !== undefined && frames !== undefined && frames > 0) stats.decodeMs = decode * 1000 / frames;
    const emitted = delta(video.jitterBufferEmittedCount, previous.jitterBufferEmittedCount);
    const jitter = delta(video.jitterBufferDelay, previous.jitterBufferDelay);
    if (jitter !== undefined && emitted !== undefined && emitted > 0) stats.jitterBufferMs = jitter * 1000 / emitted;
    stats.droppedFrames = delta(video.framesDropped, previous.framesDropped);
    return stats;
  }
}
