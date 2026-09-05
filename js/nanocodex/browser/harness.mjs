import { createWorkerEvaluator } from "../runtime/worker-evaluator.mjs";
import { browser } from "../tools/browser/index.mjs";

/** Builds the one canonical browser Agent harness used by every browser surface. */
export async function createBrowserHarness(options) {
  if (!options || typeof options !== "object" || Array.isArray(options)) {
    throw new TypeError("browser harness options must be an object");
  }
  const origin = new URL(options.origin).origin;
  if (typeof options.threadId !== "string" || !options.threadId) {
    throw new TypeError("browser harness threadId must be a non-empty string");
  }
  const retainedImages = [];
  const ownsRecentImages = options.recentImages === undefined
    && options.rememberImage === undefined;
  const runtime = await browser({
    ...options,
    origin,
    ...(ownsRecentImages ? {
      recentImages: (_sessionId, count) => retainedImages.slice(-count),
      rememberImage: (_sessionId, image) => {
        retainedImages.push(image);
        if (retainedImages.length > 5) retainedImages.splice(0, retainedImages.length - 5);
      },
    } : {}),
  });
  const now = new Date();
  const timezone = Intl.DateTimeFormat().resolvedOptions().timeZone;
  return Object.freeze({
    codeEvaluator: createWorkerEvaluator({
      egress: {
        origin,
        threadId: options.threadId,
        ...(options.headers === undefined ? {} : { headers: options.headers }),
      },
    }),
    executionEnvironment: {
      currentDate: localDate(now),
      timezone,
      ...(runtime.projectInstructions === undefined
        ? {}
        : { projectInstructions: runtime.projectInstructions }),
    },
    filesystem: runtime.filesystem,
    instructions: runtime.instructions,
    release() {
      retainedImages.length = 0;
    },
    tools: runtime.tools,
  });
}

function localDate(date) {
  return `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, "0")}-${String(date.getDate()).padStart(2, "0")}`;
}
