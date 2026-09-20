export type AttachmentUploadSigner = {
  signPart(args: {
    key: string;
    uploadId: string;
    partNumber: number;
    size: number;
    /** Canonical base64 encoding of the part's 16-byte MD5 digest. */
    md5: string;
  }): Promise<{ url: string; headers: Record<string, string>; expires_at: number }>;
};

export type AttachmentR2Config = {
  accountId: string;
  bucket: string;
  accessKeyId: string;
  secretAccessKey: string;
};

const encoder = new TextEncoder();
const expiresSeconds = 900;
const signedHeaders = "content-length;content-md5;host";
const encode = (value: string) => encodeURIComponent(value).replace(/[!'()*]/g,
  (character) => `%${character.charCodeAt(0).toString(16).toUpperCase()}`);
const hex = (bytes: ArrayBuffer) => Array.from(new Uint8Array(bytes), (byte) => byte.toString(16).padStart(2, "0")).join("");
async function hmac(key: Uint8Array, value: string): Promise<Uint8Array<ArrayBuffer>> {
  const imported = await crypto.subtle.importKey("raw", new Uint8Array(key), { name: "HMAC", hash: "SHA-256" }, false, ["sign"]);
  return new Uint8Array(await crypto.subtle.sign("HMAC", imported, encoder.encode(value)));
}

/**
 * Offline SigV4 UploadPart capabilities for the global R2 S3 endpoint.
 * expires_at is Unix milliseconds, rounded to the signature's whole-second clock.
 * The caller must authorize the key/upload/part before signing. URLs are reusable
 * bearer capabilities until expiry; do not log them.
 *
 * R2 UploadPart supports Content-MD5:
 * https://developers.cloudflare.com/r2/api/s3/api/
 * Presigning uses UNSIGNED-PAYLOAD, with the length and MD5 separately signed:
 * https://docs.aws.amazon.com/AmazonS3/latest/API/sigv4-query-string-auth.html
 */
export function createAttachmentUploadSigner(config: AttachmentR2Config): AttachmentUploadSigner {
  // Copy validated primitives: later caller mutations cannot retarget a signer.
  const { accountId, bucket, accessKeyId, secretAccessKey } = config ?? {};
  if (typeof accountId !== "string" || !/^[a-f0-9]{32}$/.test(accountId)
    || typeof bucket !== "string" || !/^[a-z0-9][a-z0-9-]{1,61}[a-z0-9]$/.test(bucket)
    || typeof accessKeyId !== "string" || !/^[a-f0-9]{32}$/.test(accessKeyId)
    || typeof secretAccessKey !== "string" || !/^[a-f0-9]{64}$/.test(secretAccessKey)) {
    throw new Error("Invalid attachment R2 signing configuration");
  }
  const host = `${accountId}.r2.cloudflarestorage.com`;
  return {
    async signPart({ key, uploadId, partNumber, size, md5 }) {
      if (typeof key !== "string" || !key || encoder.encode(key).length > 1024
        || /[\u0000-\u001f\u007f]/.test(key) || key.split("/").some((segment) => segment === "." || segment === "..")
        || typeof uploadId !== "string" || !uploadId || uploadId.length > 2048 || /[\u0000-\u001f\u007f]/.test(uploadId)
        || !Number.isInteger(partNumber) || partNumber < 1 || partNumber > 10_000
        || !Number.isSafeInteger(size) || size < 1 || size > 5 * 1024 ** 3
        || typeof md5 !== "string" || !/^[A-Za-z0-9+/]{22}==$/.test(md5) || btoa(atob(md5)) !== md5) {
        throw new Error("Invalid attachment R2 upload part");
      }
      const now = Math.floor(Date.now() / 1000) * 1000;
      const timestamp = new Date(now).toISOString().replace(/[:-]|\.\d{3}/g, "");
      const date = timestamp.slice(0, 8);
      const scope = `${date}/auto/s3/aws4_request`;
      const path = `/${bucket}/${key.split("/").map(encode).join("/")}`;
      const query = Object.entries({
        "X-Amz-Algorithm": "AWS4-HMAC-SHA256",
        "X-Amz-Credential": `${accessKeyId}/${scope}`,
        "X-Amz-Date": timestamp,
        "X-Amz-Expires": String(expiresSeconds),
        "X-Amz-SignedHeaders": signedHeaders,
        partNumber: String(partNumber),
        uploadId,
      }).map(([name, value]) => [encode(name), encode(value)]).sort(([a], [b]) => a < b ? -1 : a > b ? 1 : 0)
        .map(([name, value]) => `${name}=${value}`).join("&");
      const canonical = `PUT\n${path}\n${query}\ncontent-length:${size}\ncontent-md5:${md5}\nhost:${host}\n\n${signedHeaders}\nUNSIGNED-PAYLOAD`;
      const requestHash = hex(await crypto.subtle.digest("SHA-256", encoder.encode(canonical)));
      let signingKey = await hmac(encoder.encode(`AWS4${secretAccessKey}`), date);
      for (const component of ["auto", "s3", "aws4_request"]) signingKey = await hmac(signingKey, component);
      const signature = hex((await hmac(signingKey, `AWS4-HMAC-SHA256\n${timestamp}\n${scope}\n${requestHash}`)).buffer);
      return {
        url: `https://${host}${path}?${query}&X-Amz-Signature=${signature}`,
        headers: { "content-length": String(size), "content-md5": md5 },
        expires_at: now + expiresSeconds * 1000,
      };
    },
  };
}
