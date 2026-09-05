import { Buffer } from "buffer";
import {
  CancellationTokenSource,
  CommandRequestMessage,
  SshAuthenticationType,
  SshClientSession,
  SshSessionConfiguration,
} from "@microsoft/dev-tunnels-ssh";
import { importKey } from "@microsoft/dev-tunnels-ssh-keys";
import { defineCommand } from "just-bash/browser";

const DEFAULT_MAX_OUTPUT_BYTES = 4 * 1024 * 1024;

/**
 * Creates non-interactive SSH over a host-owned byte stream. IdentityRef is a
 * separate host boundary: the reference is forwarded, but its key never is.
 */
export function createSshCommand(options) {
  if (!options || typeof options !== "object" || Array.isArray(options)) {
    throw new TypeError("SSH options must be an object");
  }
  if (options.transport !== "tcp" && options.transport !== "websocket") {
    throw new TypeError("SSH transport must be tcp or websocket");
  }
  if (typeof options.openStream !== "function") {
    throw new TypeError("SSH openStream is required");
  }

  const capabilities = Object.freeze({
    identityFile: typeof options.readIdentity === "function",
    identityReference: typeof options.executeWithIdentityReference === "function",
    passwordReference: typeof options.resolvePassword === "function",
  });
  return defineCommand("ssh", async (args, context) => {
    if (args[0] === "--help") return ok(`${usage(options.transport, capabilities)}\n`);
    const parsed = parseArguments(args, options.transport, capabilities);
    if ("error" in parsed) return fail(`${parsed.error}\n`, 2);
    if (String(context.stdin)) {
      return fail("ssh: piped stdin is not supported by the non-interactive transport\n", 2);
    }
    try {
      if (parsed.identityReference) {
        return await options.executeWithIdentityReference({
          identityReference: parsed.identityReference,
          endpoint: parsed.endpoint,
          username: parsed.username,
          commandArgs: parsed.commandArgs,
        }, context);
      }
      return await executeSsh(parsed, options, context);
    } catch (error) {
      return fail(`ssh: ${error instanceof Error ? error.message : String(error)}\n`, 255);
    }
  });
}

/** Adapts a WHATWG byte-stream socket, including Cloudflare TCP sockets, to SSH. */
export function createWebStreamSshStream(socket, signal) {
  if (!socket?.readable || !socket?.writable || typeof socket.close !== "function") {
    throw new TypeError("SSH byte-stream socket is required");
  }
  return new WebByteStreamSshStream(socket, signal);
}

class WebByteStreamSshStream {
  #socket;
  #reader;
  #writer;
  #listeners = new Set();
  #buffer = Buffer.alloc(0);
  #disposed = false;
  #ended = false;
  #finished = false;
  #closeEvent = {};
  #signal;
  #abort;

  constructor(socket, signal) {
    this.#socket = socket;
    this.#reader = socket.readable.getReader();
    this.#writer = socket.writable.getWriter();
    this.#signal = signal;
    this.#abort = signal === undefined ? undefined : () => void this.#shutdown(signal.reason);
    signal?.addEventListener("abort", this.#abort, { once: true });
    void socket.closed?.then(
      () => this.#finish(),
      (error) => this.#finish(asError(error)),
    );
  }

  get isDisposed() {
    return this.#disposed;
  }

  closed(listener) {
    if (this.#finished) {
      queueMicrotask(() => listener(this.#closeEvent));
      return Object.freeze({ dispose() {} });
    }
    this.#listeners.add(listener);
    return Object.freeze({ dispose: () => this.#listeners.delete(listener) });
  }

  async read(count, cancellation) {
    if (this.#disposed) throw new Error("SSH socket stream is disposed");
    while (this.#buffer.byteLength === 0 && !this.#ended) {
      const result = await cancellable(this.#reader.read(), cancellation);
      if (result.done) this.#ended = true;
      else this.#buffer = Buffer.from(result.value);
    }
    if (this.#buffer.byteLength === 0) return null;
    const value = this.#buffer.subarray(0, count);
    this.#buffer = this.#buffer.subarray(value.byteLength);
    return value;
  }

  async write(data, cancellation) {
    if (this.#disposed) throw new Error("SSH socket stream is disposed");
    await cancellable(this.#writer.write(data), cancellation);
  }

  async close(error) {
    await this.#shutdown(error);
  }

  dispose() {
    void this.#shutdown();
  }

  async #shutdown(error) {
    if (this.#disposed) return;
    this.#disposed = true;
    this.#signal?.removeEventListener("abort", this.#abort);
    try {
      await this.#socket.close();
    } finally {
      this.#reader.releaseLock();
      this.#writer.releaseLock();
      this.#finish(error === undefined ? undefined : asError(error));
    }
  }

  #finish(error) {
    if (this.#finished) return;
    this.#ended = true;
    this.#finished = true;
    this.#closeEvent = error === undefined ? {} : { error };
    for (const listener of this.#listeners) listener(this.#closeEvent);
    this.#listeners.clear();
  }
}

async function executeSsh(args, options, context) {
  const cancellation = new CancellationTokenSource();
  const abort = () => cancellation.cancel();
  context.signal?.addEventListener("abort", abort, { once: true });
  let privateKey;
  let stream;
  const session = new SshClientSession(new SshSessionConfiguration());
  const authentication = session.onAuthenticating((event) => {
    if (event.authenticationType !== SshAuthenticationType.serverPublicKey || !event.publicKey) {
      event.authenticationPromise = Promise.resolve(null);
      return;
    }
    event.authenticationPromise = authenticateHost(
      event.publicKey,
      args.hostKeySha256,
      args.acceptUnknownHost,
    );
  });
  try {
    if (args.identityFile) {
      const keySource = await options.readIdentity(args.identityFile, context);
      privateKey = await importKey(keySource);
      normalizeRsaPublicKeyBlob(privateKey);
    }
    stream = await options.openStream(args.endpoint, context.signal);
    await session.connect(stream, cancellation.token);
    const credentials = privateKey
      ? { username: args.username, publicKeys: [privateKey] }
      : {
          username: args.username,
          password: await options.resolvePassword(args.passwordReference),
        };
    const serverAuthenticated = await session.authenticateServer(cancellation.token);
    if (!serverAuthenticated) throw new Error("server host-key authentication failed");

    // Some OpenSSH servers enforce the protocol ordering strictly and ignore a
    // user-authentication request sent before accepting the ssh-userauth service.
    await session.requestService("ssh-userauth", cancellation.token);
    if (!session.activateService("ssh-userauth")) {
      throw new Error("server did not activate SSH user authentication");
    }
    const authenticated = await session.authenticateClient(credentials, cancellation.token);
    if (!authenticated) throw new Error("authentication failed");

    const channel = await session.openChannel("session", cancellation.token);
    let stdout = "";
    let stderr = "";
    let outputBytes = 0;
    let outputFailure;
    const maxOutputBytes = positiveInteger(
      options.maxOutputBytes,
      DEFAULT_MAX_OUTPUT_BYTES,
      "SSH maxOutputBytes",
    );
    channel.onDataReceived((data) => {
      outputBytes += data.length;
      if (outputBytes > maxOutputBytes) {
        outputFailure ??= new Error("remote output exceeded the SSH byte limit");
        void channel.close();
        return;
      }
      stdout += data.toString("utf8");
      channel.adjustWindow(data.length);
    });
    channel.onExtendedDataReceived((event) => {
      outputBytes += event.data.length;
      if (outputBytes > maxOutputBytes) {
        outputFailure ??= new Error("remote output exceeded the SSH byte limit");
        void channel.close();
        return;
      }
      stderr += event.data.toString("utf8");
      channel.adjustWindow(event.data.length);
    });
    const closed = new Promise((resolve) => channel.onClosed(resolve));
    const request = new CommandRequestMessage();
    request.command = args.command;
    request.wantReply = true;
    if (!await channel.request(request, cancellation.token)) {
      throw new Error("remote server rejected the command");
    }
    const result = await closed;
    if (outputFailure) throw outputFailure;
    if (result.error) throw result.error;
    if (result.exitSignal) stderr += `ssh: remote command exited on ${result.exitSignal}\n`;
    return { stdout, stderr, exitCode: result.exitStatus ?? (result.exitSignal ? 128 : 0) };
  } finally {
    context.signal?.removeEventListener("abort", abort);
    authentication.dispose();
    privateKey?.dispose();
    session.dispose();
    stream?.dispose();
    cancellation.dispose();
  }
}

function normalizeRsaPublicKeyBlob(key) {
  if (key?.keyAlgorithmName !== "ssh-rsa" || typeof key.getPublicKeyBytes !== "function") return;
  const getPublicKeyBytes = key.getPublicKeyBytes.bind(key);
  // RFC 8332 changes the signature algorithm name, but the public-key blob
  // remains the RFC 4253 `ssh-rsa` encoding. The dependency otherwise writes
  // `rsa-sha2-*` into both fields, which strict OpenSSH servers reject.
  key.getPublicKeyBytes = async () => {
    try {
      return await getPublicKeyBytes("ssh-rsa");
    } catch (error) {
      // Node 24 cannot export the public KeyObject produced by the dependency's
      // private-key importer as PKCS#1, although its public JWK remains valid.
      // Encode that public material directly instead of rejecting a valid key.
      const jwk = key.publicKey?.export?.({ format: "jwk" });
      if (jwk?.kty !== "RSA" || typeof jwk.e !== "string" || typeof jwk.n !== "string") {
        throw error;
      }
      return Buffer.concat([
        sshLengthPrefixed(Buffer.from("ssh-rsa")),
        sshLengthPrefixed(sshMpint(jwk.e)),
        sshLengthPrefixed(sshMpint(jwk.n)),
      ]);
    }
  };
}

function sshMpint(base64url) {
  const value = Buffer.from(base64url, "base64url");
  let offset = 0;
  while (offset + 1 < value.length && value[offset] === 0) offset += 1;
  const normalized = value.subarray(offset);
  return normalized[0] >= 0x80 ? Buffer.concat([Buffer.from([0]), normalized]) : normalized;
}

function sshLengthPrefixed(value) {
  const length = Buffer.allocUnsafe(4);
  length.writeUInt32BE(value.length);
  return Buffer.concat([length, value]);
}

async function authenticateHost(publicKey, expected, acceptUnknown) {
  if (acceptUnknown) return {};
  const bytes = await publicKey.getPublicKeyBytes();
  if (!bytes || !expected) return null;
  const keyBytes = new Uint8Array(bytes.byteLength);
  keyBytes.set(bytes);
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", keyBytes));
  const actual = Buffer.from(digest).toString("base64").replace(/=+$/u, "");
  const normalized = expected.replace(/^SHA256:/u, "").replace(/=+$/u, "");
  return actual === normalized ? {} : null;
}

function parseArguments(args, transport, capabilities) {
  if (!args.length) return { error: usage(transport, capabilities) };
  let username = "";
  let identityFile = "";
  let identityReference = "";
  let passwordReference = "";
  let hostKeySha256;
  let acceptUnknownHost = false;
  let port = 22;
  let index = 0;
  for (; index < args.length; index += 1) {
    const arg = args[index];
    if (arg === "--") {
      index += 1;
      break;
    }
    if (!arg.startsWith("-")) break;
    if (arg === "-l" || arg === "-i" || arg === "-o" || arg === "-p") {
      const value = args[++index];
      if (!value) return { error: `ssh: ${arg} requires an argument` };
      if (arg === "-l") username = value;
      else if (arg === "-i") {
        if (!capabilities.identityFile) return { error: "ssh: identity files are unavailable" };
        identityFile = value;
      } else if (arg === "-p") {
        if (transport !== "tcp") return { error: "ssh: -p is unavailable for WebSocket endpoints" };
        port = Number(value);
        if (!Number.isInteger(port) || port < 1 || port > 65_535) {
          return { error: "ssh: port must be an integer from 1 through 65535" };
        }
      } else if (value === "StrictHostKeyChecking=no") acceptUnknownHost = true;
      else if (value.startsWith("HostKeySHA256=")) {
        hostKeySha256 = value.slice("HostKeySHA256=".length);
      } else if (value.startsWith("PasswordRef=")) {
        if (!capabilities.passwordReference) return { error: "ssh: password references are unavailable" };
        passwordReference = value.slice("PasswordRef=".length);
        if (!passwordReference) return { error: "ssh: PasswordRef cannot be empty" };
      } else if (value.startsWith("IdentityRef=")) {
        if (!capabilities.identityReference) return { error: "ssh: brokered identity references are unavailable" };
        identityReference = value.slice("IdentityRef=".length);
        if (!identityReference) return { error: "ssh: IdentityRef cannot be empty" };
      } else return { error: `ssh: unsupported option '${value}'` };
      continue;
    }
    return { error: `ssh: unsupported option '${arg}'` };
  }

  const target = args[index++];
  if (!target) return { error: usage(transport, capabilities) };
  let endpoint;
  if (transport === "websocket") {
    try { endpoint = new URL(target); } catch {
      return { error: "ssh: endpoint must be a ws:// or wss:// URL" };
    }
    if (!["ws:", "wss:"].includes(endpoint.protocol) || endpoint.username || endpoint.password) {
      return { error: "ssh: endpoint must be a credential-free ws:// or wss:// URL" };
    }
  } else {
    const parsedTarget = parseTcpTarget(target, username, port);
    if ("error" in parsedTarget) return parsedTarget;
    endpoint = parsedTarget.endpoint;
    username = parsedTarget.username;
  }

  if (!username) return { error: "ssh: provide USER@HOST or -l USER" };
  const authentication = [identityFile, identityReference, passwordReference].filter(Boolean);
  if (authentication.length === 0) {
    return { error: "ssh: provide -i PRIVATE_KEY, -o IdentityRef=REFERENCE, or -o PasswordRef=REFERENCE" };
  }
  if (authentication.length > 1) return { error: "ssh: authentication options are mutually exclusive" };
  if (identityReference && (hostKeySha256 || acceptUnknownHost)) {
    return { error: "ssh: IdentityRef uses broker-owned host-key verification" };
  }
  if (!identityReference && !hostKeySha256 && !acceptUnknownHost) {
    return { error: "ssh: provide -o HostKeySHA256=SHA256:... or explicitly -o StrictHostKeyChecking=no" };
  }
  if (args[index] === "--") index += 1;
  const commandArgs = args.slice(index);
  if (!commandArgs.length) {
    return { error: "ssh: an explicit remote command is required (interactive PTYs are unavailable)" };
  }
  return {
    endpoint,
    username,
    identityFile,
    identityReference,
    passwordReference,
    commandArgs,
    command: commandArgs.map(shellQuote).join(" "),
    hostKeySha256,
    acceptUnknownHost,
  };
}

function parseTcpTarget(target, configuredUsername, port) {
  if (/\s/u.test(target) || target.includes("://")) {
    return { error: "ssh: target must be [USER@]HOST" };
  }
  const separator = target.lastIndexOf("@");
  const targetUsername = separator === -1 ? "" : target.slice(0, separator);
  let hostname = separator === -1 ? target : target.slice(separator + 1);
  if (targetUsername && configuredUsername && targetUsername !== configuredUsername) {
    return { error: "ssh: target username conflicts with -l" };
  }
  if (hostname.startsWith("[") && hostname.endsWith("]")) hostname = hostname.slice(1, -1);
  if (!hostname || hostname.includes("@") || (!hostname.includes(":") && !/^[A-Za-z0-9.-]+$/u.test(hostname))) {
    return { error: "ssh: target must be [USER@]HOST" };
  }
  return {
    endpoint: Object.freeze({ hostname, port }),
    username: configuredUsername || targetUsername,
  };
}

function shellQuote(value) {
  return /^[A-Za-z0-9_./:@%+=,-]+$/u.test(value) ? value : `'${value.replaceAll("'", `'"'"'`)}'`;
}

function usage(transport, capabilities) {
  const endpoint = transport === "tcp" ? "[USER@]HOST" : "wss://SSH-GATEWAY";
  const port = transport === "tcp" ? " [-p PORT]" : "";
  const authentication = [
    ...(capabilities.identityFile ? ["-i PRIVATE_KEY"] : []),
    ...(capabilities.identityReference ? ["-o IdentityRef=REFERENCE"] : []),
    ...(capabilities.passwordReference ? ["-o PasswordRef=REFERENCE"] : []),
  ].join(" | ");
  const verification = capabilities.identityReference
    ? "Brokered IdentityRef records own host verification; other authentication requires HostKeySHA256 or an explicit opt-out."
    : "Provide HostKeySHA256 or explicitly disable strict host checking.";
  const transportNotice = transport === "websocket"
    ? "browsers cannot open TCP port 22; the endpoint must carry raw SSH over WebSocket."
    : "Cloudflare Workers can open direct outbound TCP; brokered identities execute inside private egress.";
  return [
    `usage: ssh${port} [-l USER] (${authentication}) ${endpoint} -- COMMAND [ARG...]`,
    "SSH is non-interactive: PTYs and piped stdin are unavailable.",
    verification,
    transportNotice,
  ].join("\n");
}

function fail(stderr, exitCode) {
  return { stdout: "", stderr, exitCode };
}

function ok(stdout) {
  return { stdout, stderr: "", exitCode: 0 };
}

function cancellable(promise, cancellation) {
  if (cancellation?.isCancellationRequested) {
    return Promise.reject(new Error("SSH command cancelled"));
  }
  if (cancellation?.onCancellationRequested === undefined) return promise;
  return new Promise((resolve, reject) => {
    let subscription;
    subscription = cancellation.onCancellationRequested(() => {
      subscription?.dispose();
      reject(new Error("SSH command cancelled"));
    });
    promise.then(
      (value) => { subscription.dispose(); resolve(value); },
      (error) => { subscription.dispose(); reject(error); },
    );
  });
}

function asError(value) {
  return value instanceof Error ? value : new Error(String(value));
}

function positiveInteger(value, fallback, name) {
  if (value === undefined) return fallback;
  if (!Number.isSafeInteger(value) || value < 1) throw new RangeError(`${name} must be a positive integer`);
  return value;
}
