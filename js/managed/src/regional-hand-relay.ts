import { DurableObject } from "cloudflare:workers";

/** Retired: regional Hand relays are gone. Kept only until its deleted_classes
 * migration ships after no Worker binds it. Serves nothing. */
export class RegionalHandRelay extends DurableObject {
  fetch(): Response { return Response.json({ error: "not_found" }, { status: 404 }); }
}
