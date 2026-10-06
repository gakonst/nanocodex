/** RFC 6238 material; only the credential broker may hold seeds or generate codes. */
export type TotpMetadata = Readonly<{
  issuer: string;
  account: string;
  origin: string;
  algorithm: "SHA1" | "SHA256" | "SHA512";
  digits: 6 | 8;
  period: number;
}>;
export type TotpMaterial = TotpMetadata & Readonly<{ seed: string }>;
const METADATA_KEYS = ["issuer", "account", "origin", "algorithm", "digits", "period"];

function label(value: unknown): value is string {
  return typeof value === "string" && value.length > 0 && value.trim() === value
    && new TextEncoder().encode(value).length <= 256 && !/[\u0000-\u001f\u007f]/.test(value);
}

export function validateTotpMetadata(value: Record<string, unknown>): TotpMetadata | undefined {
  if (!label(value.issuer) || !label(value.account) || typeof value.origin !== "string" || value.origin.length > 2048
    || typeof value.algorithm !== "string" || !["SHA1", "SHA256", "SHA512"].includes(value.algorithm)
    || (value.digits !== 6 && value.digits !== 8)
    || !Number.isInteger(value.period) || Number(value.period) < 15 || Number(value.period) > 120) return undefined;
  try {
    const origin = new URL(value.origin);
    if (origin.protocol !== "https:" || origin.origin !== value.origin || origin.username || origin.password) return undefined;
  } catch { return undefined; }
  return Object.fromEntries(METADATA_KEYS.map(key => [key, value[key]])) as TotpMetadata;
}

/** Strict base32 decoding rejects noncanonical trailing bits and malformed padding. */
function decodeSeed(seed: string): Uint8Array<ArrayBuffer> {
  if (!/^[A-Z2-7]+={0,6}$/.test(seed) || seed.length > 208) throw new Error("invalid_totp_seed");
  const raw = seed.replace(/=+$/, "");
  const remainder = raw.length % 8;
  if (![0, 2, 4, 5, 7].includes(remainder)
    || (raw.length !== seed.length && (seed.length % 8 !== 0 || remainder === 0))) throw new Error("invalid_totp_seed");
  const output = new Uint8Array(Math.floor(raw.length * 5 / 8));
  let bits = 0, accumulator = 0, offset = 0;
  for (const char of raw) {
    accumulator = (accumulator << 5) | "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567".indexOf(char);
    bits += 5;
    if (bits >= 8) {
      bits -= 8;
      output[offset++] = (accumulator >>> bits) & 255;
    }
    accumulator &= (1 << bits) - 1;
  }
  if (accumulator !== 0 || output.length < 16 || output.length > 128) throw new Error("invalid_totp_seed");
  return output;
}

/** Owner-only intake accepts a seed or an otpauth URI; public metadata omits both. */
export function validateTotpEnrollment(value: Record<string, unknown>): TotpMaterial | undefined {
  try {
    let candidate: Record<string, unknown>;
    if (Object.hasOwn(value, "otpauth_uri")) {
      if (Object.keys(value).some(key => !["name", "origin", "otpauth_uri"].includes(key))
        || typeof value.otpauth_uri !== "string" || value.otpauth_uri.length > 4096) return undefined;
      const uri = new URL(value.otpauth_uri);
      if (uri.protocol !== "otpauth:" || uri.hostname !== "totp" || uri.port || uri.username || uri.password || uri.hash || !uri.pathname.startsWith("/") || uri.pathname.slice(1).includes("/")) return undefined;
      const supported = ["secret", "issuer", "algorithm", "digits", "period"];
      for (const key of uri.searchParams.keys()) {
        if (!supported.includes(key) || uri.searchParams.getAll(key).length !== 1) return undefined;
      }
      const path = decodeURIComponent(uri.pathname.slice(1));
      const colon = path.indexOf(":");
      const labelIssuer = colon < 0 ? undefined : path.slice(0, colon);
      const issuer = uri.searchParams.get("issuer") ?? labelIssuer;
      if (labelIssuer !== undefined && issuer !== labelIssuer) return undefined;
      const numeric = (key: string, fallback: number) => {
        const text = uri.searchParams.get(key);
        return text === null ? fallback : /^[0-9]+$/.test(text) ? Number(text) : NaN;
      };
      candidate = { origin: value.origin, issuer, account: colon < 0 ? path : path.slice(colon + 1),
        seed: uri.searchParams.get("secret"), algorithm: uri.searchParams.get("algorithm") ?? "SHA1",
        digits: numeric("digits", 6), period: numeric("period", 30) };
    } else {
      if (Object.keys(value).some(key => !["name", "seed", ...METADATA_KEYS].includes(key))) return undefined;
      candidate = { algorithm: "SHA1", digits: 6, period: 30, ...value };
    }
    const metadata = validateTotpMetadata(candidate);
    if (!metadata || typeof candidate.seed !== "string") return undefined;
    const seed = candidate.seed.toUpperCase();
    decodeSeed(seed);
    return { ...metadata, seed: seed.replace(/=+$/, "") };
  } catch { return undefined; }
}

/** Time is supplied by the broker clock, never by an agent request. */
export async function generateTotp(material: TotpMaterial, epochSeconds = Date.now() / 1000): Promise<string> {
  if (!Number.isFinite(epochSeconds) || epochSeconds < 0) throw new Error("invalid_totp_time");
  const step = Math.floor(epochSeconds / material.period);
  if (!Number.isSafeInteger(step)) throw new Error("invalid_totp_time");
  const counter = new ArrayBuffer(8);
  new DataView(counter).setBigUint64(0, BigInt(step));
  const hash = { SHA1: "SHA-1", SHA256: "SHA-256", SHA512: "SHA-512" }[material.algorithm];
  const seed = decodeSeed(material.seed);
  try {
    const key = await crypto.subtle.importKey("raw", seed, { name: "HMAC", hash }, false, ["sign"]);
    const mac = new Uint8Array(await crypto.subtle.sign("HMAC", key, counter));
    const offset = mac[mac.length - 1]! & 15;
    const truncated = new DataView(mac.buffer).getUint32(offset) & 0x7fffffff;
    return String(truncated % 10 ** material.digits).padStart(material.digits, "0");
  } finally { seed.fill(0); }
}
