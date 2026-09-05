import { createHmac, randomBytes, timingSafeEqual } from "node:crypto";

const PROOF_WINDOW_MS = 30_000;
const NONCE = /^[a-f0-9]{32}$/;
const MAC = /^[a-f0-9]{64}$/;

export type AuthCapabilityProof = {
  at_ms: number;
  nonce: string;
  mac: string;
};

export function createCapabilityProof(operation: string): AuthCapabilityProof {
  const atMs = Date.now();
  const nonce = randomBytes(16).toString("hex");
  return {
    at_ms: atMs,
    nonce,
    mac: sign(operation, atMs, nonce),
  };
}

export function verifyCapabilityProof(
  candidate: AuthCapabilityProof,
  operation: string,
  usedNonces: Map<string, number>,
): void {
  const now = Date.now();
  for (const [nonce, expiresAt] of usedNonces) {
    if (expiresAt <= now) usedNonces.delete(nonce);
  }
  if (
    !candidate ||
    typeof candidate !== "object" ||
    !Number.isSafeInteger(candidate.at_ms) ||
    Math.abs(now - candidate.at_ms) > PROOF_WINDOW_MS ||
    typeof candidate.nonce !== "string" ||
    !NONCE.test(candidate.nonce) ||
    typeof candidate.mac !== "string" ||
    !MAC.test(candidate.mac) ||
    usedNonces.has(candidate.nonce)
  ) {
    throw new Error("credential actor capability proof was rejected");
  }

  const actual = Buffer.from(candidate.mac, "hex");
  const expected = Buffer.from(sign(operation, candidate.at_ms, candidate.nonce), "hex");
  if (!timingSafeEqual(actual, expected)) {
    throw new Error("credential actor capability proof was rejected");
  }
  usedNonces.set(candidate.nonce, now + PROOF_WINDOW_MS);
}

function sign(operation: string, atMs: number, nonce: string): string {
  const secret = capabilitySecret();
  return createHmac("sha256", secret)
    .update(operation)
    .update("\0")
    .update(String(atMs))
    .update("\0")
    .update(nonce)
    .digest("hex");
}

function capabilitySecret(): string {
  const secret = process.env.NANOCODEX_AUTH_CAPABILITY;
  if (!secret?.trim()) throw new Error("NANOCODEX_AUTH_CAPABILITY is not configured");
  if (Buffer.byteLength(secret, "utf8") < 32) {
    throw new Error("NANOCODEX_AUTH_CAPABILITY must be at least 32 bytes");
  }
  return secret;
}
