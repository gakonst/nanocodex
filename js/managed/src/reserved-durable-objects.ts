import { DurableObject } from "cloudflare:workers";

// A v4 experiment created these production namespaces before its source was
// merged. Keep their data intact while the supported managed runtime remains
// on the committed account/connector surface. No application route uses them.
class ReservedDurableObject extends DurableObject {
  async fetch(): Promise<Response> {
    return Response.json({ error: "not_found" }, { status: 404 });
  }
}

export class MemoryScope extends ReservedDurableObject {}

export class Organization extends ReservedDurableObject {}
