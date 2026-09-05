import { writeFile } from "node:fs/promises";

await Promise.all([
  writeFile(new URL("../pkg-node/package.json", import.meta.url), '{"type":"commonjs"}\n'),
  writeFile(new URL("../pkg-web/package.json", import.meta.url), '{"type":"module"}\n'),
]);
