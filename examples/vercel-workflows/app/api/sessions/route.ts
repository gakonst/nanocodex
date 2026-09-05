import { start } from "workflow/api";

import { hasBearerToken } from "@/lib/bearer-auth";
import { nanocodexActor } from "@/workflows/nanocodex-actor";
import type { DurabilityPortableStateArchive } from "nanocodex/durability";

export const runtime = "nodejs";

export async function POST(request: Request): Promise<Response> {
  if (!authorizedToCreate(request)) {
    return Response.json(
      { error: { code: "unauthorized", message: "session creation token was rejected" } },
      { status: 401, headers: { "cache-control": "no-store" } },
    );
  }
  try {
    const archive = await optionalDurabilityArchive(request);
    const run = archive === undefined
      ? await start(nanocodexActor)
      : await start(nanocodexActor, [archive]);
    return Response.json(
      {
        session_id: run.runId,
        durability_id: archive?.stateId ?? run.runId,
      },
      { status: 201, headers: { "cache-control": "no-store" } },
    );
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    return Response.json(
      { error: { code: "session_start_failed", message } },
      { status: 500, headers: { "cache-control": "no-store" } },
    );
  }
}

async function optionalDurabilityArchive(
  request: Request,
): Promise<DurabilityPortableStateArchive | undefined> {
  const encoded = await request.text();
  if (!encoded.trim()) return undefined;
  const body = JSON.parse(encoded) as { durability?: unknown };
  if (!body || typeof body !== "object" || Array.isArray(body)
    || Object.keys(body).some((key) => key !== "durability")
    || body.durability === undefined) {
    throw new TypeError("session creation body must contain only durability");
  }
  return body.durability as DurabilityPortableStateArchive;
}

function authorizedToCreate(request: Request): boolean {
  const expected = process.env.NANOCODEX_ADMIN_TOKEN?.trim();
  if (!expected) return true;
  return hasBearerToken(request, expected);
}
