import type { RepositoryView } from "./threadRepository.ts";

const encoder = new TextEncoder();
const decoder = new TextDecoder();
const maxSidebandDataBytes = 65_515;
const zeroOid = "0".repeat(40);

export type PacketLine =
  | { kind: "data"; data: Uint8Array }
  | { kind: "flush" }
  | { kind: "delimiter" };

export type ReceiveCommand = {
  oldOid: string;
  newOid: string;
  ref: string;
};

export function encodePacketLine(value: string | Uint8Array): Uint8Array {
  const payload = typeof value === "string" ? encoder.encode(value) : value;
  const length = payload.byteLength + 4;
  if (length > 0xffff) throw new Error("packet line exceeds 65535 bytes");
  const packet = new Uint8Array(length);
  packet.set(encoder.encode(length.toString(16).padStart(4, "0")));
  packet.set(payload, 4);
  return packet;
}

export function parsePacketLines(value: Uint8Array): PacketLine[] {
  const packets: PacketLine[] = [];
  let offset = 0;
  while (offset < value.byteLength) {
    const parsed = parsePacketAt(value, offset);
    packets.push(parsed.packet);
    offset = parsed.offset;
  }
  return packets;
}

export function repositoryAdvertisement(): Uint8Array {
  return concatenate([
    encodePacketLine("version 2\n"),
    encodePacketLine("agent=nanocodex-cloudflare/1\n"),
    encodePacketLine("ls-refs=unborn\n"),
    encodePacketLine("fetch\n"),
    encodePacketLine("object-format=sha1\n"),
    flushPacket,
  ]);
}

export function legacyRepositoryAdvertisement(repository: RepositoryView | undefined): Uint8Array {
  const capabilities = [
    "side-band-64k",
    "ofs-delta",
    "no-thin",
    ...(repository ? [`symref=HEAD:refs/heads/${repository.branch}`] : []),
    "agent=nanocodex-cloudflare/1",
  ].join(" ");
  const refs = repository?.refs ?? [];
  return concatenate([
    encodePacketLine("# service=git-upload-pack\n"),
    flushPacket,
    ...(repository
      ? [
          encodePacketLine(`${repository.head} HEAD\0${capabilities}\n`),
          ...refs.map((ref) => encodePacketLine(`${ref.oid} ${ref.name}\n`)),
        ]
      : [encodePacketLine(`${zeroOid} capabilities^{}\0${capabilities}\n`)]),
    flushPacket,
  ]);
}

export function receiveAdvertisement(repository?: RepositoryView): Uint8Array {
  const capabilities = [
    "report-status",
    "side-band-64k",
    "ofs-delta",
    "agent=nanocodex-cloudflare/1",
  ].join(" ");
  return concatenate([
    encodePacketLine("# service=git-receive-pack\n"),
    flushPacket,
    encodePacketLine(repository
      ? `${repository.head} refs/heads/${repository.branch}\0${capabilities}\n`
      : `${zeroOid} capabilities^{}\0${capabilities}\n`),
    flushPacket,
  ]);
}

export function parseV2Command(body: Uint8Array): {
  command: string | null;
  arguments: string[];
} {
  const packets = parsePacketLines(body);
  let command: string | null = null;
  let inArguments = false;
  const arguments_: string[] = [];
  for (const packet of packets) {
    if (packet.kind === "delimiter") {
      inArguments = true;
      continue;
    }
    if (packet.kind !== "data") continue;
    const line = decoder.decode(packet.data).replace(/\n$/, "");
    if (!inArguments && line.startsWith("command=")) {
      command = line.slice("command=".length);
    } else if (inArguments) {
      arguments_.push(line);
    }
  }
  return { command, arguments: arguments_ };
}

export function parseReceiveRequest(body: Uint8Array): {
  commands: ReceiveCommand[];
  pack: Uint8Array;
  reportStatus: boolean;
  sideBand64k: boolean;
} {
  const commandLines: string[] = [];
  let offset = 0;
  while (offset < body.byteLength) {
    const parsed = parsePacketAt(body, offset);
    offset = parsed.offset;
    if (parsed.packet.kind === "flush") break;
    if (parsed.packet.kind !== "data") {
      throw new Error("unexpected receive-pack delimiter");
    }
    commandLines.push(decoder.decode(parsed.packet.data).replace(/\r?\n$/, ""));
  }
  if (commandLines.length === 0) throw new Error("receive-pack omitted ref commands");

  const nul = commandLines[0]!.indexOf("\0");
  const capabilities = nul < 0 ? "" : commandLines[0]!.slice(nul + 1).trim();
  if (nul >= 0) commandLines[0] = commandLines[0]!.slice(0, nul);
  const commands = commandLines.map((line) => {
    const parts = line.trim().split(/\s+/);
    if (parts.length !== 3) throw new Error("invalid receive-pack command");
    return { oldOid: parts[0]!, newOid: parts[1]!, ref: parts[2]! };
  });
  return {
    commands,
    pack: body.subarray(offset),
    reportStatus: capabilities.split(/\s+/).includes("report-status"),
    sideBand64k: capabilities.split(/\s+/).includes("side-band-64k"),
  };
}

export function buildLsRefsResponse(
  repository: RepositoryView | undefined,
  arguments_: readonly string[],
): Uint8Array {
  if (!repository) return flushPacket.slice();
  const prefixes = arguments_
    .filter((argument) => argument.startsWith("ref-prefix "))
    .map((argument) => argument.slice("ref-prefix ".length));
  const refs = repository.refs.filter(
    (ref) => prefixes.length === 0 || prefixes.some((prefix) => ref.name.startsWith(prefix)),
  );
  const headRef = `refs/heads/${repository.branch}`;
  const head = repository.refs.find((ref) => ref.name === headRef) ?? {
    name: headRef,
    oid: repository.head,
  };
  return concatenate([
    encodePacketLine(`${head.oid} HEAD symref-target:${headRef}\n`),
    ...refs.map((ref) => encodePacketLine(`${ref.oid} ${ref.name}\n`)),
    flushPacket,
  ]);
}

export function buildNegotiationResponse(commonHaves: readonly string[] = []): Uint8Array {
  return concatenate([
    encodePacketLine("acknowledgments\n"),
    ...(commonHaves.length === 0
      ? [encodePacketLine("NAK\n")]
      : commonHaves.map((oid) => encodePacketLine(`ACK ${oid}\n`))),
    flushPacket,
  ]);
}

export function buildReceiveReport(command: ReceiveCommand, error?: string): Uint8Array {
  return concatenate([
    encodePacketLine(error ? `unpack error ${error}\n` : "unpack ok\n"),
    encodePacketLine(error ? `ng ${command.ref} ${error}\n` : `ok ${command.ref}\n`),
    flushPacket,
  ]);
}

export function buildSidebandReceiveResponse(report: Uint8Array): Uint8Array {
  const payload = new Uint8Array(report.byteLength + 1);
  payload[0] = 1;
  payload.set(report, 1);
  return concatenate([encodePacketLine(payload), flushPacket]);
}

export function buildFullPackResponse(
  pack: ReadableStream<Uint8Array>,
): ReadableStream<Uint8Array> {
  return buildPackResponse(pack, "packfile\n");
}

export function buildLegacyFullPackResponse(
  pack: ReadableStream<Uint8Array>,
  acknowledgedHave?: string,
): ReadableStream<Uint8Array> {
  return buildPackResponse(pack, acknowledgedHave ? `ACK ${acknowledgedHave}\n` : "NAK\n");
}

function buildPackResponse(
  pack: ReadableStream<Uint8Array>,
  prelude: string,
): ReadableStream<Uint8Array> {
  const reader = pack.getReader();
  let chunk: Uint8Array<ArrayBufferLike> = new Uint8Array(0);
  let chunkOffset = 0;
  let finished = false;
  return new ReadableStream<Uint8Array>({
    start(controller) {
      controller.enqueue(encodePacketLine(prelude));
    },
    async pull(controller) {
      if (finished) return;
      if (chunkOffset >= chunk.byteLength) {
        const next = await reader.read();
        if (next.done) {
          finished = true;
          controller.enqueue(flushPacket);
          controller.close();
          return;
        }
        chunk = next.value;
        chunkOffset = 0;
      }
      const end = Math.min(chunkOffset + maxSidebandDataBytes, chunk.byteLength);
      const payload = new Uint8Array(1 + end - chunkOffset);
      payload[0] = 1;
      payload.set(chunk.subarray(chunkOffset, end), 1);
      chunkOffset = end;
      controller.enqueue(encodePacketLine(payload));
    },
    cancel(reason) {
      return reader.cancel(reason);
    },
  });
}

export const flushPacket = encoder.encode("0000");

function parsePacketAt(value: Uint8Array, start: number): {
  packet: PacketLine;
  offset: number;
} {
  if (start + 4 > value.byteLength) throw new Error("truncated packet prefix");
  const rawLength = decoder.decode(value.subarray(start, start + 4));
  if (!/^[0-9a-fA-F]{4}$/.test(rawLength)) throw new Error("invalid packet prefix");
  const length = Number.parseInt(rawLength, 16);
  if (length === 0) return { packet: { kind: "flush" }, offset: start + 4 };
  if (length === 1) return { packet: { kind: "delimiter" }, offset: start + 4 };
  if (length < 4 || start + length > value.byteLength) {
    throw new Error("truncated packet payload");
  }
  return {
    packet: { kind: "data", data: value.slice(start + 4, start + length) },
    offset: start + length,
  };
}

function concatenate(chunks: readonly Uint8Array[]): Uint8Array {
  const result = new Uint8Array(chunks.reduce((total, chunk) => total + chunk.byteLength, 0));
  let offset = 0;
  for (const chunk of chunks) {
    result.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return result;
}
