import { createNanocodexVitePlugin } from "./plugin.mjs";

/** Internal dependency seam used by provider-free tests. Not package-exported. */
export function createNanocodexCloudflarePlugins(options, cloudflare, integration = {}) {
  let devBindings;
  const credentialBrokerWorker = options.chatGpt === false
    ? undefined
    : options.chatGpt?.credentialBrokerWorker;
  const core = createNanocodexVitePlugin(
    {
      chatGpt: options.chatGpt,
      devApplications: options.devApplications,
      oauthRelay: options.oauthRelay,
    },
    {
      ...integration,
      target: "cloudflare",
      setDevBindings(value) {
        devBindings = value;
      },
    },
  );
  return [
    core,
    ...cloudflare(withDevBindings(
      options.cloudflare ?? {},
      () => devBindings,
      credentialBrokerWorker,
    )),
  ];
}

function withDevBindings(options, getDevBindings, credentialBrokerWorker) {
  if (credentialBrokerWorker) {
    return {
      ...options,
      auxiliaryWorkers: options.auxiliaryWorkers?.map((worker) => ({
        ...worker,
        config(workerConfig, context) {
          const customized = typeof worker.config === "function"
            ? worker.config(workerConfig, context)
            : worker.config;
          if (
            workerConfig.name !== credentialBrokerWorker
            && workerConfig.topLevelName !== credentialBrokerWorker
          ) return customized;
          const bindings = getDevBindings();
          if (!bindings) return customized;
          return {
            ...(customized ?? {}),
            vars: {
              ...(workerConfig.vars ?? {}),
              ...(customized?.vars ?? {}),
              ...bindings,
            },
          };
        },
      })),
    };
  }
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
          ...(workerConfig.vars ?? {}),
          ...(customized?.vars ?? {}),
          ...bindings,
        },
      };
    },
  };
}
