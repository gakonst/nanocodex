import { createClient } from "rivetkit/client";

import type { registry } from "./registry.js";

export function createNanocodexClient(endpoint = "http://127.0.0.1:6420") {
  return createClient<typeof registry>({ endpoint });
}
