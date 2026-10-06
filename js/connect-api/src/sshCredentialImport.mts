export const sshCredentialImportResourcePrefix = "urn:nanocodex:credential-import:ssh:pem-v1:sha256:";
export const sshTargetResourcePrefix = "urn:nanocodex:ssh-target:";
const encoder = new TextEncoder();
export type SshTarget = Readonly<{ reference: string; hostname: string; port: number; username: string; host_key_sha256: string }>;
export type SshCredentialImport = SshTarget & Readonly<{ private_key: string }>;

export function parseSshTarget(value: unknown): SshTarget {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Invalid SSH target.");
  const v = value as Record<string, unknown>;
  if (typeof v.reference !== "string" || !/^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/.test(v.reference)
    || ["__proto__", "constructor", "prototype"].includes(v.reference)
    || typeof v.hostname !== "string" || !validHostname(v.hostname)
    || typeof v.username !== "string" || !/^[A-Za-z0-9._-]{1,128}$/.test(v.username)
    || typeof v.host_key_sha256 !== "string" || !/^SHA256:[A-Za-z0-9+/]{43}=?$/.test(v.host_key_sha256)
    || !Number.isInteger(v.port) || (v.port as number) < 1 || (v.port as number) > 65535) throw new Error("Invalid SSH target.");
  const resource = `${sshTargetResourcePrefix}${encodeURIComponent(v.reference)}:${encodeURIComponent(v.hostname)}:${v.port}:${encodeURIComponent(v.username)}:${encodeURIComponent(v.host_key_sha256)}`;
  if (resource.length > 512) throw new Error("The SSH target exceeds the signed resource limit.");
  return { reference: v.reference, hostname: v.hostname, username: v.username, host_key_sha256: v.host_key_sha256, port: v.port as number };
}

function validHostname(host: string): boolean {
  try { if (new URL(`https://${host}`).hostname !== host) return false; }
  catch { return false; }
  if (host.length > 253 || !host.includes(".") || !host.split(".").every(label => /^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/.test(label))
    || /\.(localhost|internal|invalid|local|test|home\.arpa)$/.test(host)) return false;
  if (/^[0-9.]+$/.test(host)) {
    const parts = host.split(".");
    if (parts.length !== 4 || parts.some(p => !/^(0|[1-9][0-9]{0,2})$/.test(p) || Number(p) > 255)) return false;
    const [a, b] = parts.map(Number) as [number, number, number, number];
    if (a === 0 || a === 10 || a === 127 || a >= 224 || (a === 100 && b >= 64 && b <= 127)
      || (a === 169 && b === 254) || (a === 172 && b >= 16 && b <= 31)
      || (a === 192 && (b === 0 || b === 168)) || (a === 198 && (b === 18 || b === 19))) return false;
  }
  return true;
}

export function parseSshCredentialImport(value: unknown): SshCredentialImport {
  const target = parseSshTarget(value);
  const v = value as Record<string, unknown>;
  const expected = ["reference", "hostname", "port", "username", "host_key_sha256", "private_key"];
  const key = v.private_key;
  if (Object.keys(v).length !== expected.length || Object.keys(v).some(k => !expected.includes(k))
    || typeof key !== "string" || encoder.encode(key).length > 64 * 1024
    || !/^-----BEGIN (RSA PRIVATE KEY|EC PRIVATE KEY|PRIVATE KEY)-----\r?\n[A-Za-z0-9+/=\r\n]+\r?\n-----END \1-----(?:\r?\n)?$/.test(key)) throw new Error("Invalid SSH credential.");
  return Object.freeze({ ...target, private_key: key });
}

export function sshTargetResource(value: SshTarget): string {
  const v = parseSshTarget(value);
  return `${sshTargetResourcePrefix}${encodeURIComponent(v.reference)}:${encodeURIComponent(v.hostname)}:${v.port}:${encodeURIComponent(v.username)}:${encodeURIComponent(v.host_key_sha256)}`;
}

export function parseSshTargetResource(resource: unknown): SshTarget | undefined {
  if (typeof resource !== "string" || !resource.startsWith(sshTargetResourcePrefix)) return undefined;
  try {
    const fields = resource.slice(sshTargetResourcePrefix.length).split(":").map(decodeURIComponent);
    if (fields.length !== 5) return undefined;
    const target = parseSshTarget({ reference: fields[0], hostname: fields[1], port: Number(fields[2]), username: fields[3], host_key_sha256: fields[4] });
    return sshTargetResource(target) === resource ? target : undefined;
  } catch { return undefined; }
}

export function isAllowedSshCredentialImportResource(resource: unknown): boolean {
  return typeof resource === "string" && resource.startsWith(sshCredentialImportResourcePrefix)
    && /^[A-Za-z0-9_-]{43}$/.test(resource.slice(sshCredentialImportResourcePrefix.length));
}

export function sshImportFromResources(resources: readonly string[]): { digest: string; target: SshTarget } | undefined {
  const imports = resources.filter(r => r.startsWith("urn:nanocodex:credential-import:ssh:"));
  const targets = resources.filter(r => r.startsWith(sshTargetResourcePrefix));
  if (!imports.length && !targets.length) return undefined;
  const target = parseSshTargetResource(targets[0]);
  if (imports.length !== 1 || targets.length !== 1 || !isAllowedSshCredentialImportResource(imports[0]) || !target) throw new Error("Invalid signed SSH import resources.");
  return { digest: imports[0]!.slice(sshCredentialImportResourcePrefix.length), target };
}

export async function sshCredentialImportDigest(value: unknown): Promise<string> {
  const v = parseSshCredentialImport(value);
  const domain = encoder.encode("nanocodex/ssh-credential-import/v1\0");
  const fields = [v.reference, v.hostname, v.username, v.host_key_sha256, v.private_key].map(f => encoder.encode(f));
  const bytes = new Uint8Array(domain.length + fields.reduce((n, f) => n + 4 + f.length, 0) + 4);
  const view = new DataView(bytes.buffer);
  bytes.set(domain);
  let offset = domain.length;
  for (const field of fields) { view.setUint32(offset, field.length, false); offset += 4; bytes.set(field, offset); offset += field.length; }
  view.setUint32(offset, v.port, false);
  const hash = new Uint8Array(await crypto.subtle.digest("SHA-256", bytes));
  return btoa(String.fromCharCode(...hash)).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}
