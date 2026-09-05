import { durabilityRevision, exportDurabilityStatePage } from "nanocodex/durability";
import type { DurabilityExportPageRequest } from "nanocodex/durability";

import { hasBearerToken } from "@/lib/bearer-auth";
import { errorResponse, RequestError } from "@/lib/validation";
import { postgresDurabilityStore } from "@/workflows/postgres-durability";

export const runtime = "nodejs";

export async function POST(request: Request): Promise<Response> {
  const expected = process.env.NANOCODEX_ADMIN_TOKEN?.trim();
  if (expected && !hasBearerToken(request, expected)) {
    return Response.json(
      { error: { code: "unauthorized", message: "durability export token was rejected" } },
      { status: 401, headers: { "cache-control": "no-store" } },
    );
  }
  try {
    const body = parseExportRequest(await request.json());
    return Response.json(
      await exportDurabilityStatePage(postgresDurabilityStore(), body.state_id, {
        from: body.from,
        ...(body.fromDigest === undefined ? {} : { fromDigest: body.fromDigest }),
        to: body.to,
        cursor: body.cursor,
        limit: body.limit,
      }),
      { headers: { "cache-control": "no-store" } },
    );
  } catch (error) {
    if (error instanceof RequestError) return errorResponse(error);
    const message = error instanceof Error ? error.message : String(error);
    return Response.json(
      { error: { code: "durability_export_failed", message } },
      { status: 500, headers: { "cache-control": "no-store" } },
    );
  }
}

type ExportRequest = DurabilityExportPageRequest & Readonly<{ state_id: string }>;

function parseExportRequest(value: unknown): ExportRequest {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw invalidRequest("durability export body must be a JSON object");
  }
  const body = value as Record<string, unknown>;
  const fields = ["state_id", "from", "fromDigest", "to", "cursor", "limit"];
  if (Object.keys(body).some((key) => !fields.includes(key))) {
    throw invalidRequest("durability export body contains an unknown field");
  }
  if (typeof body.state_id !== "string" || !body.state_id.trim()) {
    throw invalidRequest("state_id must be a non-empty string");
  }

  const from = parseRevision(body.from, "from");
  const to = body.to === undefined ? undefined : parseRevision(body.to, "to");
  const fromDigest = body.fromDigest;
  if (fromDigest !== undefined
    && (typeof fromDigest !== "string" || !/^sha256:[0-9a-f]{64}$/.test(fromDigest))) {
    throw invalidRequest("fromDigest must be a SHA-256 durability state digest");
  }
  if (from !== "0" && fromDigest === undefined) {
    throw invalidRequest("fromDigest is required when from is nonzero");
  }
  if (body.cursor !== undefined
    && (typeof body.cursor !== "string" || !/^v1:(0|[1-9][0-9]*)$/.test(body.cursor))) {
    throw invalidRequest("cursor is invalid");
  }
  if (body.limit !== undefined
    && (!Number.isSafeInteger(body.limit) || (body.limit as number) < 1
      || (body.limit as number) > 1024 * 1024)) {
    throw invalidRequest("limit must be an integer from 1 to 1048576");
  }

  return {
    state_id: body.state_id,
    from,
    ...(fromDigest === undefined ? {} : { fromDigest }),
    to,
    cursor: body.cursor as string | undefined,
    limit: body.limit as number | undefined,
  };
}

function parseRevision(value: unknown, field: string): ReturnType<typeof durabilityRevision> {
  if (typeof value !== "string" && typeof value !== "number") {
    throw invalidRequest(`${field} must be an unsigned 64-bit decimal revision`);
  }
  try {
    return durabilityRevision(value);
  } catch {
    throw invalidRequest(`${field} must be an unsigned 64-bit decimal revision`);
  }
}

function invalidRequest(message: string): RequestError {
  return new RequestError("invalid_request", message);
}
