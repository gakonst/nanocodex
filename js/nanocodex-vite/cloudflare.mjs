import { readFile } from "node:fs/promises";
import { createRequire } from "node:module";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";

import { createNanocodexCloudflarePlugins } from "./cloudflare-plugin.mjs";

const projectUrl = pathToFileURL(resolve(process.cwd(), "package.json"));
const cloudflareModule = await import(await resolveProjectImport("@cloudflare/vite-plugin"));
const { cloudflare } = cloudflareModule;

/** One-call Nanocodex and Cloudflare integration for development and deployment. */
export function nanocodex(options = {}) {
  return createNanocodexCloudflarePlugins(options, cloudflare);
}

async function resolveProjectImport(packageName) {
  const projectRequire = createRequire(projectUrl);
  for (const modulesDirectory of projectRequire.resolve.paths(packageName) ?? []) {
    const packageFile = resolve(modulesDirectory, packageName, "package.json");
    try {
      const manifest = JSON.parse(await readFile(packageFile, "utf8"));
      const target = manifest.exports?.["."]?.import ?? manifest.module;
      if (typeof target === "string") {
        return pathToFileURL(resolve(packageFile, "..", target)).href;
      }
    } catch (error) {
      if (error?.code !== "ENOENT") throw error;
    }
  }
  throw new Error(`${packageName} must be installed by the Vite application`);
}
