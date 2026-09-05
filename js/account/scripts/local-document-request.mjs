/**
 * Vite owns local application documents before the Cloudflare Worker/API
 * middleware. Browsers advertise HTML explicitly, while ordinary command-line
 * probes use a missing or wildcard Accept header. Permit that generic form
 * only when the caller has already established an exact document route.
 */
export function isLocalDocumentRequest(request, allowGeneric = false) {
  const method = request.method ?? "GET";
  if (method !== "GET" && method !== "HEAD") return false;

  const accept = request.headers.accept?.toLowerCase();
  if (accept?.includes("text/html")) return true;
  if (!allowGeneric
    || request.headers["sec-fetch-dest"] !== undefined
    || request.headers["sec-fetch-mode"] !== undefined) return false;
  return accept === undefined || accept.includes("*/*");
}
