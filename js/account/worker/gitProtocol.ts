import type {
  RepositoryPublication,
  RepositoryRef,
} from "./gitRepository.ts";

const encoder = new TextEncoder();
const decoder = new TextDecoder();
const maxSidebandDataBytes = 65_515;

export type PacketLine =
  | { kind: "data"; data: Uint8Array }
  | { kind: "flush" }
  | { kind: "delimiter" };

export function encodePacketLine(value: string | Uint8Array): Uint8Array {
  const payload = typeof value === "string" ? encoder.encode(value) : value;
  const length = payload.byteLength + 4;
  if (length > 0xffff) throw new Error("packet line exceeds 65535 bytes");
  const prefix = encoder.encode(length.toString(16).padStart(4, "0"));
  const packet = new Uint8Array(length);
  packet.set(prefix);
  packet.set(payload, 4);
  return packet;
}

export function parsePacketLines(value: Uint8Array): PacketLine[] {
  const packets: PacketLine[] = [];
  let offset = 0;
  while (offset < value.byteLength) {
    if (offset + 4 > value.byteLength) throw new Error("truncated packet prefix");
    const rawLength = decoder.decode(value.subarray(offset, offset + 4));
    if (!/^[0-9a-f]{4}$/.test(rawLength)) throw new Error("invalid packet prefix");
    const length = Number.parseInt(rawLength, 16);
    offset += 4;
    if (length === 0) {
      packets.push({ kind: "flush" });
      continue;
    }
    if (length === 1) {
      packets.push({ kind: "delimiter" });
      continue;
    }
    if (length < 4 || offset + length - 4 > value.byteLength) {
      throw new Error("truncated packet payload");
    }
    packets.push({
      kind: "data",
      data: value.slice(offset, offset + length - 4),
    });
    offset += length - 4;
  }
  return packets;
}

export function repositoryAdvertisement(): Uint8Array {
  return concatenate([
    encodePacketLine("version 2\n"),
    encodePacketLine("agent=nanocodex-cloudflare/1\n"),
    encodePacketLine("ls-refs=unborn\n"),
    encodePacketLine("fetch=shallow\n"),
    encodePacketLine("object-format=sha1\n"),
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

export function buildLsRefsResponse(
  publication: RepositoryPublication,
  arguments_: readonly string[],
): Uint8Array {
  const prefixes = arguments_
    .filter((argument) => argument.startsWith("ref-prefix "))
    .map((argument) => argument.slice("ref-prefix ".length));
  const refs = publication.refs.filter(
    (ref) => prefixes.length === 0 || prefixes.some((prefix) => ref.name.startsWith(prefix)),
  );
  const includePeeled = arguments_.includes("peel");
  const headRef = `refs/heads/${publication.branch}`;
  const head = publication.refs.find((ref) => ref.name === headRef) ?? {
    name: headRef,
    oid: publication.head,
  };
  return concatenate([
    encodePacketLine(`${head.oid} HEAD symref-target:${headRef}\n`),
    ...refs.map((ref) => formatRefPacket(ref, includePeeled)),
    flushPacket,
  ]);
}

function formatRefPacket(ref: RepositoryRef, includePeeled: boolean): Uint8Array {
  const peeled = includePeeled && ref.peeled != null ? ` peeled:${ref.peeled}` : "";
  return encodePacketLine(`${ref.oid} ${ref.name}${peeled}\n`);
}

export type GitFetchRequest = {
  wants: string[];
  haves: string[];
  shallow: string[];
  deepen: number;
  deepenRelative: boolean;
  done: boolean;
};

export function parseFetchArguments(arguments_: readonly string[]): GitFetchRequest {
  const request: GitFetchRequest = {
    wants: [],
    haves: [],
    shallow: [],
    deepen: 0,
    deepenRelative: false,
    done: false,
  };
  for (const argument of arguments_) {
    if (argument.startsWith("want ")) request.wants.push(parseOid(argument.slice(5), "want"));
    else if (argument.startsWith("have ")) request.haves.push(parseOid(argument.slice(5), "have"));
    else if (argument.startsWith("shallow ")) {
      request.shallow.push(parseOid(argument.slice(8), "shallow"));
    } else if (argument.startsWith("deepen ")) {
      const depth = Number(argument.slice(7));
      if (!Number.isSafeInteger(depth) || depth <= 0 || depth > 0x7fffffff) {
        throw new Error("invalid deepen argument");
      }
      request.deepen = depth;
    } else if (argument === "done") request.done = true;
    else if (argument === "deepen-relative") request.deepenRelative = true;
    else if (
      argument.startsWith("deepen-since ") ||
      argument.startsWith("deepen-not ")
    ) {
      throw new Error("unsupported shallow fetch mode");
    }
  }
  return request;
}

export function buildNegotiationResponse(commonHaves: readonly string[]): Uint8Array {
  return concatenate([
    encodePacketLine("acknowledgments\n"),
    ...(commonHaves.length === 0
      ? [encodePacketLine("NAK\n")]
      : commonHaves.map((oid) => encodePacketLine(`ACK ${oid}\n`))),
    flushPacket,
  ]);
}

export function buildFullPackResponse(
  pack: ReadableStream<Uint8Array>,
  shallow: readonly string[] = [],
  unshallow: readonly string[] = [],
): ReadableStream<Uint8Array> {
  const reader = pack.getReader();
  let chunk: Uint8Array<ArrayBufferLike> = new Uint8Array(0);
  let chunkOffset = 0;
  let finished = false;
  return new ReadableStream<Uint8Array>({
    start(controller) {
      if (shallow.length > 0 || unshallow.length > 0) {
        controller.enqueue(encodePacketLine("shallow-info\n"));
        for (const oid of shallow) controller.enqueue(encodePacketLine(`shallow ${oid}\n`));
        for (const oid of unshallow) controller.enqueue(encodePacketLine(`unshallow ${oid}\n`));
        controller.enqueue(delimiterPacket);
      }
      controller.enqueue(encodePacketLine("packfile\n"));
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
export const delimiterPacket = encoder.encode("0001");

function parseOid(value: string, field: string): string {
  if (!/^[a-f0-9]{40}$/.test(value)) throw new Error(`invalid ${field} object id`);
  return value;
}

function concatenate(chunks: readonly Uint8Array[]): Uint8Array {
  const length = chunks.reduce((total, chunk) => total + chunk.byteLength, 0);
  const result = new Uint8Array(length);
  let offset = 0;
  for (const chunk of chunks) {
    result.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return result;
}
