import { Buffer } from "./browserBuffer.mjs";
import { CancellationTokenSource, CommandRequestMessage, SshAuthenticationType, SshClientSession, SshSessionConfiguration, WebSocketStream, } from "./devTunnelsSshBrowser.mjs";
import { importKey } from "@microsoft/dev-tunnels-ssh-keys";
import { defineCommand } from "nanocodex-tools/just-bash/browser";
export function createSshCommand(filesystem) {
    return defineCommand("ssh", async (args, context) => {
        if (args[0] === "--help")
            return ok(`${usage()}\n`);
        const parsed = parseArguments(args);
        if ("error" in parsed)
            return fail(`${parsed.error}\n`, 2);
        if (String(context.stdin)) {
            return fail("ssh: piped stdin is not supported by the browser WebSocket transport\n", 2);
        }
        try {
            return await executeSsh(parsed, filesystem, context);
        }
        catch (error) {
            return fail(`ssh: ${error instanceof Error ? error.message : String(error)}\n`, 255);
        }
    });
}
async function executeSsh(args, filesystem, context) {
    const keyPath = filesystem.resolvePath(context.cwd, args.identityFile);
    const keySource = await filesystem.readFile(keyPath);
    const privateKey = await importKey(keySource);
    const websocket = new WebSocket(args.endpoint);
    websocket.binaryType = "arraybuffer";
    const cancellation = new CancellationTokenSource();
    const abort = () => {
        cancellation.cancel();
        websocket.close(1000, "command cancelled");
    };
    context.signal?.addEventListener("abort", abort, { once: true });
    const session = new SshClientSession(new SshSessionConfiguration());
    const authentication = session.onAuthenticating((event) => {
        if (event.authenticationType !== SshAuthenticationType.serverPublicKey || !event.publicKey) {
            event.authenticationPromise = Promise.resolve(null);
            return;
        }
        event.authenticationPromise = authenticateHost(event.publicKey, args.hostKeySha256, args.acceptUnknownHost);
    });
    try {
        await waitForWebSocket(websocket, context.signal);
        await session.connect(new WebSocketStream(websocket), cancellation.token);
        const authenticated = await session.authenticate({
            username: args.username,
            publicKeys: [privateKey],
        }, cancellation.token);
        if (!authenticated)
            throw new Error("authentication failed");
        const channel = await session.openChannel("session", cancellation.token);
        let stdout = "";
        let stderr = "";
        channel.onDataReceived((data) => {
            stdout += data.toString("utf8");
            channel.adjustWindow(data.length);
        });
        channel.onExtendedDataReceived((event) => {
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
        if (result.error)
            throw result.error;
        if (result.exitSignal)
            stderr += `ssh: remote command exited on ${result.exitSignal}\n`;
        return { stdout, stderr, exitCode: result.exitStatus ?? (result.exitSignal ? 128 : 0) };
    }
    finally {
        context.signal?.removeEventListener("abort", abort);
        authentication.dispose();
        privateKey.dispose();
        session.dispose();
        cancellation.dispose();
        if (websocket.readyState === WebSocket.OPEN)
            websocket.close(1000, "command complete");
    }
}
async function authenticateHost(publicKey, expected, acceptUnknown) {
    if (acceptUnknown)
        return {};
    const bytes = await publicKey.getPublicKeyBytes();
    if (!bytes || !expected)
        return null;
    const keyBytes = new Uint8Array(bytes.byteLength);
    keyBytes.set(bytes);
    const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", keyBytes));
    let binary = "";
    for (const byte of digest)
        binary += String.fromCharCode(byte);
    const actual = btoa(binary).replace(/=+$/, "");
    const normalized = expected.replace(/^SHA256:/, "").replace(/=+$/, "");
    return actual === normalized ? {} : null;
}
function parseArguments(args) {
    if (!args.length)
        return { error: usage() };
    let username = "";
    let identityFile = "";
    let hostKeySha256;
    let acceptUnknownHost = false;
    let index = 0;
    for (; index < args.length; index += 1) {
        const arg = args[index];
        if (arg === "--") {
            index += 1;
            break;
        }
        if (!arg.startsWith("-"))
            break;
        if (arg === "-l" || arg === "-i" || arg === "-o") {
            const value = args[++index];
            if (!value)
                return { error: `ssh: ${arg} requires an argument` };
            if (arg === "-l")
                username = value;
            else if (arg === "-i")
                identityFile = value;
            else if (value === "StrictHostKeyChecking=no")
                acceptUnknownHost = true;
            else if (value.startsWith("HostKeySHA256="))
                hostKeySha256 = value.slice("HostKeySHA256=".length);
            else
                return { error: `ssh: unsupported browser option '${value}'` };
            continue;
        }
        return { error: `ssh: unsupported browser option '${arg}'` };
    }
    const endpointText = args[index++];
    if (!endpointText)
        return { error: usage() };
    let endpoint;
    try {
        endpoint = new URL(endpointText);
    }
    catch {
        return { error: "ssh: endpoint must be a ws:// or wss:// URL" };
    }
    if (!["ws:", "wss:"].includes(endpoint.protocol) || endpoint.username || endpoint.password) {
        return { error: "ssh: endpoint must be a credential-free ws:// or wss:// URL" };
    }
    if (!username)
        return { error: "ssh: -l USER is required" };
    if (!identityFile)
        return { error: "ssh: -i PRIVATE_KEY is required" };
    if (!hostKeySha256 && !acceptUnknownHost) {
        return { error: "ssh: provide -o HostKeySHA256=SHA256:... or explicitly -o StrictHostKeyChecking=no" };
    }
    const commandArgs = args.slice(index);
    if (!commandArgs.length)
        return { error: "ssh: an explicit remote command is required (interactive PTYs are unavailable)" };
    return {
        endpoint,
        username,
        identityFile,
        command: commandArgs.map(shellQuote).join(" "),
        hostKeySha256,
        acceptUnknownHost,
    };
}
function shellQuote(value) {
    return /^[A-Za-z0-9_./:@%+=,-]+$/.test(value) ? value : `'${value.replaceAll("'", `'"'"'`)}'`;
}
function waitForWebSocket(websocket, signal) {
    return new Promise((resolve, reject) => {
        const cleanup = () => {
            websocket.removeEventListener("open", onOpen);
            websocket.removeEventListener("error", onError);
            signal?.removeEventListener("abort", onAbort);
        };
        const onOpen = () => { cleanup(); resolve(); };
        const onError = () => { cleanup(); reject(new Error("WebSocket connection failed")); };
        const onAbort = () => { cleanup(); reject(signal?.reason ?? new Error("command cancelled")); };
        websocket.addEventListener("open", onOpen, { once: true });
        websocket.addEventListener("error", onError, { once: true });
        signal?.addEventListener("abort", onAbort, { once: true });
        if (signal?.aborted)
            onAbort();
    });
}
function usage() {
    return [
        "usage: ssh -l USER -i PRIVATE_KEY (-o HostKeySHA256=SHA256:... |",
        "           -o StrictHostKeyChecking=no) wss://SSH-GATEWAY -- COMMAND [ARG...]",
        "Browser SSH requires a server-provided WebSocket carrying raw SSH; browsers cannot open TCP port 22.",
    ].join("\n");
}
function fail(stderr, exitCode) {
    return { stdout: "", stderr, exitCode };
}
function ok(stdout) {
    return { stdout, stderr: "", exitCode: 0 };
}
