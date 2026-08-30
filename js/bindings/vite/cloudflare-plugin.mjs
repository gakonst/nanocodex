import { createNanocodexVitePlugin } from "./plugin.mjs";

/** Internal dependency seam used by provider-free tests. Not package-exported. */
export function createNanocodexCloudflarePlugins(options, cloudflare) {
  let devBindings;
  const core = createNanocodexVitePlugin(
    { chatGpt: options.chatGpt, webMcp: options.webMcp },
    {
      target: "cloudflare",
      setDevBindings(value) {
        devBindings = value;
      },
    },
  );
  return [core, ...cloudflare(withDevBindings(options.cloudflare ?? {}, () => devBindings))];
}

function withDevBindings(options, getDevBindings) {
  const userConfig = options.config;
  return {
    ...options,
    config(workerConfig) {
      const customized = typeof userConfig === "function"
        ? userConfig(workerConfig)
        : userConfig;
      const bindings = getDevBindings();
      if (!bindings) return customized;
      return {
        ...(customized ?? {}),
        vars: {
          ...(customized?.vars ?? {}),
          ...bindings,
        },
      };
    },
  };
}
