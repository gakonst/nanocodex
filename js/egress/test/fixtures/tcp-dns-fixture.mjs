import { connect as socketConnect } from "cloudflare:sockets";

// DNS fixture: the synthetic public target resolves to the local test sshd.
// Every other destination is refused, so no test traffic leaves the host.
export function connect(address, options) {
  if (address.hostname !== "sshd.example.com") throw new Error("unexpected TCP destination");
  return socketConnect({ hostname: "127.0.0.1", port: address.port }, options);
}
