// Rust owns the source policy; Go embeds this generated copy in standalone builds.
import { copyFile } from "node:fs/promises";
await copyFile(
  new URL("../crates/nanocodex-hand/src/capture_policy.json", import.meta.url),
  new URL("../hands/remote/capture_policy.json", import.meta.url),
);
