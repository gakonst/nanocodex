const DNS_NAME = /^(?=.{1,253}$)(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z](?:[a-z0-9-]{0,61}[a-z0-9])?$/;
const PRIVATE_SUFFIXES = [".internal", ".invalid", ".local", ".localhost", ".test", ".home.arpa"];

/**
 * Turns the one supported bare MCP host into its endpoint and verifies HTTPS
 * URLs before either Account or the broker persists them.
 */
export function canonicalRemoteMcpTarget(value: unknown): Readonly<{
  endpoint: string;
  name: string;
}> {
  if (typeof value !== "string" || value.length < 1 || value.length > 2_048) {
    throw new Error("Remote MCP target must be a bounded public host or HTTPS URL.");
  }
  let endpoint: URL;
  if (value === "mcp.linear.app") {
    endpoint = new URL("https://mcp.linear.app/mcp");
  } else {
    try { endpoint = new URL(value); } catch { throw new Error("Remote MCP target is invalid."); }
  }
  const hostname = endpoint.hostname.toLowerCase();
  if (endpoint.protocol !== "https:" || endpoint.username || endpoint.password || endpoint.hash
    || !DNS_NAME.test(hostname) || hostname === "localhost"
    || PRIVATE_SUFFIXES.some((suffix) => hostname.endsWith(suffix))) {
    throw new Error("Remote MCP target must use a public HTTPS endpoint.");
  }
  if (endpoint.port && endpoint.port !== "443") {
    throw new Error("Remote MCP target cannot use a custom port.");
  }
  endpoint.hostname = hostname;
  endpoint.port = "";
  if (endpoint.pathname === "/") endpoint.pathname = "/mcp";
  if (endpoint.pathname.length > 1_024) throw new Error("Remote MCP target is too large.");
  if (endpoint.search) throw new Error("Remote MCP target cannot contain a query string.");
  return Object.freeze({
    endpoint: endpoint.href,
    name: value === "mcp.linear.app" ? value : hostname,
  });
}
