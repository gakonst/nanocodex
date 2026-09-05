import { namedTool } from "./namedTool.mjs";
import { DATASET_DESCRIPTION, datasetOutputSchema, datasetParameters } from "./datasetContract.mjs";

/** A session-scoped, lazy browser dataset inspector. */
export function dataset(options = {}) {
  if (!options || typeof options !== "object" || Array.isArray(options)) {
    throw new TypeError("dataset tool options must be an object");
  }
  if (options.fetch !== undefined && typeof options.fetch !== "function") {
    throw new TypeError("dataset tool fetch must be a function");
  }
  const engineOptions = Object.freeze({ fetch: options.fetch });
  let loaded;
  let resolved;
  let disposed = false;
  const releasedSessions = new Set();
  const tool = namedTool("dataset", {
    description: DATASET_DESCRIPTION,
    parameters: datasetParameters,
    outputSchema: datasetOutputSchema,
    handler(input, context) {
      if (disposed) throw new Error("dataset tool is disposed");
      loaded ??= import("./datasetEngine.mjs")
        .then(({ createDatasetTool }) => {
          resolved = createDatasetTool(engineOptions);
          for (const sessionId of releasedSessions) resolved.releaseSession(sessionId);
          releasedSessions.clear();
          if (disposed) resolved.dispose();
          return resolved;
        });
      return loaded.then((tool) => {
        if (disposed) throw new Error("dataset tool is disposed");
        return tool.handler(input, context);
      });
    },
    releaseSession(sessionId) {
      if (resolved) resolved.releaseSession(sessionId);
      else if (loaded) releasedSessions.add(sessionId);
    },
    dispose() {
      disposed = true;
      releasedSessions.clear();
      resolved?.dispose();
    },
  });
  return tool;
}
