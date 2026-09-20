import { createHash, createHmac } from "node:crypto";
import { afterEach, expect, it, vi } from "vitest";
import { createAttachmentUploadSigner, type AttachmentR2Config } from "../src/attachment-r2";

const config: AttachmentR2Config = {
  accountId: "0123456789abcdef0123456789abcdef",
  bucket: "attachment-fixtures",
  accessKeyId: "fedcba9876543210fedcba9876543210",
  secretAccessKey: "0123456789abcdef".repeat(4),
};
const args = {
  key: "brains/test-agent/attachments/test-upload/original.mp4",
  uploadId: "opaque+/upload=id",
  partNumber: 2,
  size: 5_242_880,
  md5: createHash("md5").update("fixture part").digest("base64"),
};

// Independent Node crypto verifier; also checked against AWS's published SigV4
// query authentication example, rather than only comparing this signer to itself.
function referenceSignature(url: string, headers: Record<string, string>, secret: string, method = "PUT") {
  const parsed = new URL(url);
  const query = parsed.searchParams;
  const names = query.get("X-Amz-SignedHeaders")!;
  const scope = query.get("X-Amz-Credential")!.split("/").slice(1);
  const escape = (text: string) => [...new TextEncoder().encode(text)].map((byte) =>
    /[A-Za-z0-9_~.-]/.test(String.fromCharCode(byte)) ? String.fromCharCode(byte) : `%${byte.toString(16).toUpperCase().padStart(2, "0")}`).join("");
  const sortedQuery = [...query].filter(([key]) => key !== "X-Amz-Signature")
    .map(([key, value]) => `${escape(key)}=${escape(value)}`).sort().join("&");
  const allHeaders = { ...headers, host: parsed.host };
  const headerLines = names.split(";").map((name) => `${name}:${allHeaders[name as keyof typeof allHeaders] ?? ""}\n`).join("");
  const canonical = [method, parsed.pathname, sortedQuery, headerLines, names, "UNSIGNED-PAYLOAD"].join("\n");
  const toSign = ["AWS4-HMAC-SHA256", query.get("X-Amz-Date"), scope.join("/"), createHash("sha256").update(canonical).digest("hex")].join("\n");
  let key: Buffer = Buffer.from(`AWS4${secret}`);
  for (const value of scope) key = createHmac("sha256", key).update(value).digest();
  return createHmac("sha256", key).update(toSign).digest("hex");
}

afterEach(() => vi.restoreAllMocks());

it("validates the reference against AWS's published presigned GET vector", () => {
  // https://docs.aws.amazon.com/AmazonS3/latest/API/sigv4-query-string-auth.html
  const url = "https://examplebucket.s3.amazonaws.com/test.txt?X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential=AKIAIOSFODNN7EXAMPLE%2F20130524%2Fus-east-1%2Fs3%2Faws4_request&X-Amz-Date=20130524T000000Z&X-Amz-Expires=86400&X-Amz-SignedHeaders=host";
  expect(referenceSignature(url, {}, "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY", "GET"))
    .toBe("aeeed9bbccd4d02ee5c0109b86d86835f995330da4c265957d157751f604d404");
});

it("signs a deterministic R2 UploadPart with bound length, MD5 and a 900-second expiry", async () => {
  vi.spyOn(Date, "now").mockReturnValue(Date.parse("2026-09-20T12:34:56.789Z"));
  const signer = createAttachmentUploadSigner(config);
  const signed = await signer.signPart(args);
  expect(await signer.signPart(args)).toEqual(signed);
  const url = new URL(signed.url);
  expect(url.origin).toBe(`https://${config.accountId}.r2.cloudflarestorage.com`);
  expect(url.pathname).toBe(`/${config.bucket}/${args.key}`);
  expect(Object.fromEntries(url.searchParams)).toMatchObject({
    "X-Amz-Algorithm": "AWS4-HMAC-SHA256",
    "X-Amz-Credential": `${config.accessKeyId}/20260920/auto/s3/aws4_request`,
    "X-Amz-Date": "20260920T123456Z",
    "X-Amz-Expires": "900",
    "X-Amz-SignedHeaders": "content-length;content-md5;host",
    partNumber: "2", uploadId: args.uploadId,
  });
  expect(signed.headers).toEqual({ "content-length": String(args.size), "content-md5": args.md5 });
  expect(signed.expires_at).toBe(Date.parse("2026-09-20T12:49:56Z"));
  expect(url.searchParams.get("X-Amz-Signature")).toBe(referenceSignature(signed.url, signed.headers, config.secretAccessKey));
  expect(Object.keys(signed).sort()).toEqual(["expires_at", "headers", "url"]);
  expect(JSON.stringify(signed)).not.toContain(config.secretAccessKey);
});

it("invalidates the signature when method, endpoint, bucket, key, upload, part, expiry or headers change", async () => {
  const signed = await createAttachmentUploadSigner(config).signPart(args);
  const original = new URL(signed.url).searchParams.get("X-Amz-Signature");
  const changedUrls = [
    signed.url.replace(config.accountId, "a".repeat(32)),
    signed.url.replace(config.bucket, "other-bucket"),
    signed.url.replace("original.mp4", "other.mp4"),
    signed.url.replace("partNumber=2", "partNumber=3"),
    signed.url.replace("uploadId=", "uploadId=other"),
    signed.url.replace("X-Amz-Expires=900", "X-Amz-Expires=901"),
  ];
  for (const url of changedUrls) expect(referenceSignature(url, signed.headers, config.secretAccessKey)).not.toBe(original);
  expect(referenceSignature(signed.url, signed.headers, config.secretAccessKey, "GET")).not.toBe(original);
  for (const headers of [
    { ...signed.headers, "content-length": String(args.size + 1) },
    { ...signed.headers, "content-md5": "AAAAAAAAAAAAAAAAAAAAAA==" },
    { "content-length": String(args.size) },
  ]) expect(referenceSignature(signed.url, headers, config.secretAccessKey)).not.toBe(original);
});

it("encodes opaque paths and upload IDs without changing their scope", async () => {
  const key = "brains/a//literal %2F/雪 !'()*?#.png";
  const signed = await createAttachmentUploadSigner(config).signPart({ ...args, key, uploadId: "x+/=&?#雪" });
  const url = new URL(signed.url);
  expect(decodeURIComponent(url.pathname)).toBe(`/${config.bucket}/${key}`);
  expect(url.pathname).toContain("%252F");
  expect(url.pathname).toContain("%21%27%28%29%2A%3F%23");
  expect(url.searchParams.get("uploadId")).toBe("x+/=&?#雪");
  expect(url.searchParams.get("X-Amz-Signature")).toBe(referenceSignature(signed.url, signed.headers, config.secretAccessKey));
});

it("copies config and rejects malformed configuration without leaking values", async () => {
  const mutable = { ...config };
  const signer = createAttachmentUploadSigner(mutable);
  mutable.bucket = "different-bucket";
  expect(new URL((await signer.signPart(args)).url).pathname).toContain(`/${config.bucket}/`);
  for (const [field, values] of Object.entries({
    accountId: ["", "x".repeat(32), `${config.accountId}.evil.example`, ` ${config.accountId}`],
    bucket: ["", "ab", "a".repeat(64), "Bad-Bucket", "bad/path", "bad.bucket", "-bad"],
    accessKeyId: ["", "secret-value", `${config.accessKeyId}\n`],
    secretAccessKey: ["", "bad secret value", "g".repeat(64)],
  })) for (const value of values) {
    expect(() => createAttachmentUploadSigner({ ...config, [field]: value })).toThrow("Invalid attachment R2 signing configuration");
  }
});

it("rejects invalid part bounds, noncanonical digests and path normalization hazards", async () => {
  const signer = createAttachmentUploadSigner(config);
  for (const patch of [
    { key: "" }, { key: "a/../b" }, { key: "a/./b" }, { key: "a\nb" }, { key: "雪".repeat(342) },
    { uploadId: "" }, { uploadId: "bad\r\nvalue" }, { uploadId: "a".repeat(2049) },
    { partNumber: 0 }, { partNumber: 10_001 }, { partNumber: 1.5 }, { partNumber: NaN },
    { size: 0 }, { size: -1 }, { size: 0.5 }, { size: 5 * 1024 ** 3 + 1 }, { size: Infinity },
    { md5: "0".repeat(32) }, { md5: "AAAAAAAAAAAAAAAAAAAAAB==" }, { md5: `${args.md5}\n` },
  ]) await expect(signer.signPart({ ...args, ...patch })).rejects.toThrow("Invalid attachment R2 upload part");
  await expect(signer.signPart({ ...args, size: 1, partNumber: 1 })).resolves.toBeDefined();
  await expect(signer.signPart({ ...args, size: 5 * 1024 ** 3, partNumber: 10_000 })).resolves.toBeDefined();
});
