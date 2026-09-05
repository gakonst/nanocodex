export function redactPublicText(value) {
  return String(value)
    .replace(/-----BEGIN [^-]+ PRIVATE KEY-----[\s\S]*?-----END [^-]+ PRIVATE KEY-----/gi, "[private key redacted]")
    .replace(/\bBearer\s+\S+/gi, "Bearer [redacted]")
    .replace(/\b(?:sk|sess)-[A-Za-z0-9_-]{12,}\b/g, "[credential]")
    .replace(/\b((?:OPENAI|ANTHROPIC|AWS|GITHUB|GH|TAILSCALE)_[A-Z0-9_]*(?:KEY|TOKEN|SECRET)=)\S+/g, "$1[redacted]")
    .replace(/\/(?:home|Users)\/[^/\s]+(?:\/[^\s"'`)]*)?/g, "[host-path]")
    .replace(/\/mnt\/[^/\s]+(?:\/[^\s"'`)]*)?/g, "[host-path]")
    .replace(/\b[a-z0-9-]+\.tail[a-z0-9-]*\.ts\.net\b/gi, "[tailnet-host]")
    .replace(/\bdev-[a-z0-9-]+\b/gi, "[dev-host]")
    .replace(/\b(?:\d{1,3}\.){3}\d{1,3}(?::\d+)?\b/g, "[network-address]");
}
