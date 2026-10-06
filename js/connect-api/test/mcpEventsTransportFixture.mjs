import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { createServer as httpsServer } from "node:https";
import os from "node:os";
import path from "node:path";
import { promisify } from "node:util";

const execute = promisify(execFile);

/** Run the whole public journey in a private Linux user/network/mount namespace.
 * No host interfaces, DNS files or application transport are changed. workerd's
 * real network service supports TLS upgrade; its external HTTP CONNECT service
 * does not. Unshare, iproute2, mount and openssl are test prerequisites. */
export async function runInNetworkNamespace(t, filename) {
  if (process.env.MCP_EVENTS_NETWORK_NAMESPACE === "1") return false;
  assert.equal(process.platform, "linux", "MCP Events network journey requires Linux user namespaces");
  const childEnv = { ...process.env, MCP_EVENTS_NETWORK_NAMESPACE: "1" };
  delete childEnv.NODE_TEST_CONTEXT;
  try {
    const { stdout, stderr } = await execute("unshare", ["--user", "--map-root-user", "--net", "--mount", "--",
      process.execPath, "--test", filename], {
      env: childEnv, timeout: 175_000, maxBuffer: 4 * 1024 * 1024,
    });
    t.diagnostic(stdout); if (stderr) t.diagnostic(stderr);
  } catch (error) {
    t.diagnostic(error.stdout ?? ""); t.diagnostic(error.stderr ?? "");
    throw error;
  }
  return true;
}

/** External DNS/TLS fixture only. The Worker runs its unchanged DNS validation,
 * numeric-IP connection, TLS hostname check, framing, signatures and DO alarms.
 * Synthetic public addresses exist only inside the test's network namespace. */
export async function createTransportFixture({ onRequest }) {
  assert.equal(process.env.MCP_EVENTS_NETWORK_NAMESPACE, "1", "Run this fixture through runInNetworkNamespace");
  const directory = await mkdtemp(path.join(os.tmpdir(), "mcp-events-tls-"));
  await execute("ip", ["link", "set", "lo", "up"]);
  for (const address of ["93.184.216.34", "93.184.216.35"]) await execute("ip", ["addr", "add", `${address}/32`, "dev", "lo"]);
  const hosts = path.join(directory, "hosts");
  await writeFile(hosts, `${await readFile("/etc/hosts", "utf8")}\n93.184.216.35 cloudflare-dns.com\n`);
  await execute("mount", ["--bind", hosts, "/etc/hosts"]);
  const keyPath = path.join(directory, "key.pem"), certificatePath = path.join(directory, "cert.pem");
  await execute("openssl", ["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "2",
    "-subj", "/CN=callbacks.mcp-events.example", "-addext", "subjectAltName=DNS:callbacks.mcp-events.example,DNS:cloudflare-dns.com",
    "-keyout", keyPath, "-out", certificatePath]);
  const [key, cert] = await Promise.all([readFile(keyPath), readFile(certificatePath)]);
  const sockets = new Set(), trace = [];
  const callback = httpsServer({ key, cert }, async (incoming, outgoing) => {
    try {
      assert.equal(incoming.socket.servername, "callbacks.mcp-events.example", "TLS preserves the callback SNI hostname");
      const chunks = [];
      for await (const chunk of incoming) chunks.push(chunk);
      const request = new Request(`https://${incoming.headers.host}${incoming.url}`, {
        method: incoming.method, headers: incoming.headers,
        ...(chunks.length ? { body: Buffer.concat(chunks) } : {}),
      });
      trace.push({ kind: "callback", path: new URL(request.url).pathname, servername: incoming.socket.servername });
      const response = await onRequest(request);
      outgoing.writeHead(response.status, Object.fromEntries(response.headers));
      outgoing.end(Buffer.from(await response.arrayBuffer()));
    } catch (error) { outgoing.writeHead(500); outgoing.end(String(error)); }
  });
  const resolver = httpsServer({ key, cert }, (incoming, outgoing) => {
    const url = new URL(`https://${incoming.headers.host}${incoming.url}`);
    trace.push({ kind: "dns", hostname: url.searchParams.get("name"), type: url.searchParams.get("type"), servername: incoming.socket.servername });
    if (url.hostname !== "cloudflare-dns.com" || url.pathname !== "/dns-query") { outgoing.writeHead(403); outgoing.end(); return; }
    const name = url.searchParams.get("name"), type = url.searchParams.get("type");
    const address = ["callbacks.mcp-events.example", "mismatch.mcp-events.example"].includes(name) ? "93.184.216.34" : "127.0.0.1";
    const body = JSON.stringify({ Status: 0, Answer: type === "A" ? [{ name, type: 1, TTL: 30, data: address }] : [] });
    outgoing.writeHead(200, { "content-type": "application/dns-json", "content-length": Buffer.byteLength(body) }); outgoing.end(body);
  });
  callback.on("connection", socket => trace.push({ kind: "connect", target: `${socket.localAddress}:${socket.localPort}` }));
  callback.on("tlsClientError", error => trace.push({ kind: "tls-rejected", message: error.message }));
  for (const server of [callback, resolver]) server.on("connection", socket => { sockets.add(socket); socket.on("close", () => sockets.delete(socket)); });
  await Promise.all([[callback, "93.184.216.34"], [resolver, "93.184.216.35"]].map(([server, address]) => new Promise((resolve, reject) => {
    server.once("error", reject); server.listen(443, address, resolve);
  })));
  return {
    trace,
    miniflareOptions: { outboundService: { network: { allow: ["public"], tlsOptions: { trustBrowserCas: false, trustedCertificates: [cert.toString()] } } } },
    async close() {
      for (const socket of sockets) socket.destroy();
      await Promise.all([callback, resolver].map(server => new Promise(resolve => server.close(resolve))));
      await execute("umount", ["/etc/hosts"]);
      await rm(directory, { recursive: true, force: true });
    },
  };
}
