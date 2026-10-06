import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { createServer as httpsServer } from "node:https";
import os from "node:os";
import path from "node:path";
import { Readable } from "node:stream";
import { promisify } from "node:util";

const execute = promisify(execFile);

/** Run the whole public journey in a private Linux user/network/mount namespace.
 * No host interfaces, DNS files or application transport are changed. workerd's
 * real public-only network service resolves fixture hostnames and validates TLS.
 * Unshare, iproute2, mount and openssl are test prerequisites. */
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

/** External DNS/TLS fixture only. The Worker runs its unchanged global fetch transport,
 * URL validation, TLS hostname check, signatures and DO alarms.
 * Synthetic public addresses exist only inside the test's network namespace. */
export async function createTransportFixture({ onRequest }) {
  assert.equal(process.env.MCP_EVENTS_NETWORK_NAMESPACE, "1", "Run this fixture through runInNetworkNamespace");
  const directory = await mkdtemp(path.join(os.tmpdir(), "mcp-events-tls-"));
  await execute("ip", ["link", "set", "lo", "up"]);
  for (const address of ["93.184.216.34", "10.0.0.1", "169.254.169.254"]) await execute("ip", ["addr", "add", `${address}/32`, "dev", "lo"]);
  const hosts = path.join(directory, "hosts");
  await writeFile(hosts, `${await readFile("/etc/hosts", "utf8")}\n93.184.216.34 callbacks.mcp-events.example transport-checks.mcp-events.example mismatch.mcp-events.example\n127.0.0.1 private.mcp-events.example\n10.0.0.1 rfc1918.mcp-events.example\n169.254.169.254 metadata.mcp-events.example\n`);
  await execute("mount", ["--bind", hosts, "/etc/hosts"]);
  const keyPath = path.join(directory, "key.pem"), certificatePath = path.join(directory, "cert.pem");
  await execute("openssl", ["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "2",
    "-subj", "/CN=callbacks.mcp-events.example", "-addext", "subjectAltName=DNS:callbacks.mcp-events.example,DNS:transport-checks.mcp-events.example,DNS:private.mcp-events.example,DNS:rfc1918.mcp-events.example,DNS:metadata.mcp-events.example",
    "-keyout", keyPath, "-out", certificatePath]);
  const [key, cert] = await Promise.all([readFile(keyPath), readFile(certificatePath)]);
  const sockets = new Set(), trace = [];
  const callback = httpsServer({ key, cert }, async (incoming, outgoing) => {
    try {
      assert.equal(incoming.socket.servername, incoming.headers.host, "TLS preserves the callback SNI hostname");
      const chunks = [];
      for await (const chunk of incoming) chunks.push(chunk);
      const request = new Request(`https://${incoming.headers.host}${incoming.url}`, {
        method: incoming.method, headers: incoming.headers,
        ...(chunks.length ? { body: Buffer.concat(chunks) } : {}),
      });
      trace.push({ kind: "callback", path: new URL(request.url).pathname, servername: incoming.socket.servername });
      const response = await onRequest(request);
      outgoing.writeHead(response.status, Object.fromEntries(response.headers));
      outgoing.flushHeaders();
      if (response.body) {
        const body = Readable.fromWeb(response.body);
        outgoing.on("close", () => body.destroy());
        body.on("error", error => outgoing.destroy(error));
        body.pipe(outgoing);
      } else outgoing.end();
    } catch (error) { outgoing.writeHead(500); outgoing.end(String(error)); }
  });
  callback.on("connection", socket => trace.push({ kind: "connect", target: `${socket.localAddress}:${socket.localPort}` }));
  callback.on("tlsClientError", error => trace.push({ kind: "tls-rejected", message: error.message }));
  callback.on("connection", socket => { sockets.add(socket); socket.on("close", () => sockets.delete(socket)); });
  await new Promise((resolve, reject) => {
    callback.once("error", reject); callback.listen(443, "0.0.0.0", resolve);
  });
  return {
    trace,
    miniflareOptions: { outboundService: { network: { allow: ["public"], tlsOptions: { trustBrowserCas: false, trustedCertificates: [cert.toString()] } } } },
    async close() {
      for (const socket of sockets) socket.destroy();
      await new Promise(resolve => callback.close(resolve));
      await execute("umount", ["/etc/hosts"]);
      await rm(directory, { recursive: true, force: true });
    },
  };
}
