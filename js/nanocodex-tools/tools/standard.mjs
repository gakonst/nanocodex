import { IMAGE_DESCRIPTION, WEB_DESCRIPTION } from "./standardDescriptions.mjs";
import { namedTool } from "./namedTool.mjs";
import { toolResult } from "../runtime/code-runtime.mjs";

export { namedTool };

const MAX_VIEW_IMAGE_BYTES = 10 * 1024 * 1024;
const DEFAULT_WEB_URL = "/api/tools/web-search";
const DEFAULT_IMAGE_URL = "/api/tools/image-generation";

export function web(options = {}) {
  const request = jsonRequester(options, DEFAULT_WEB_URL);
  return namedTool("web__run", {
    supportsParallelToolCalls: true,
    description: WEB_DESCRIPTION,
    parameters: webParameters,
    outputSchema: { type: "string" },
    async handler(input, context) {
      const commands = normalizeWebCommands(input);
      const result = requireObject(await request({
        commands,
        session_id: context.sessionId,
        model: context.model,
      }, context.signal), "web__run response");
      return requireString(result.output, "web__run response.output");
    },
  });
}

export function imageGeneration(options = {}) {
  const request = jsonRequester(options, DEFAULT_IMAGE_URL);
  const recentImages = options.recentImages ?? (() => []);
  const rememberImage = options.rememberImage ?? (() => {});
  return namedTool("image_gen__imagegen", {
    description: IMAGE_DESCRIPTION,
    parameters: {
      type: "object",
      properties: {
        prompt: { type: "string" },
        referenced_image_paths: {
          type: ["array", "null"],
          items: { type: "string" },
          maxItems: 5,
        },
        num_last_images_to_include: {
          type: ["integer", "null"],
          minimum: 1,
          maximum: 5,
        },
      },
      required: ["prompt"],
      additionalProperties: false,
    },
    outputSchema: {
      type: "object",
      properties: {
        image_url: { type: "string" },
        output_hint: { type: "string" },
      },
      required: ["image_url"],
      additionalProperties: true,
    },
    async handler(input, context) {
      const args = requireObject(input, "image_gen__imagegen");
      const prompt = requireString(args.prompt, "image_gen__imagegen.prompt");
      const paths = optionalStringArray(args.referenced_image_paths);
      const count = optionalInteger(args.num_last_images_to_include);
      if (paths.length > 5) throw new Error("referenced_image_paths accepts at most 5 paths");
      if (paths.length && count !== undefined) {
        throw new Error("provide referenced_image_paths or num_last_images_to_include, not both");
      }
      if (count !== undefined && (count < 1 || count > 5)) {
        throw new Error("num_last_images_to_include must be between 1 and 5");
      }
      const images = paths.length
        ? await Promise.all(paths.map((path) => workspaceImage(options.workspace, path)))
        : count === undefined ? [] : recentImages(context.sessionId, count);
      if (count !== undefined && images.length !== count) {
        throw new Error(`requested ${count} recent images, but only ${images.length} are available`);
      }
      const result = requireObject(
        await request({ images, prompt }, context.signal),
        "image_gen__imagegen response",
      );
      const imageUrl = requireString(result.image_url, "image_gen__imagegen response.image_url");
      rememberImage(context.sessionId, imageUrl);
      return result;
    },
  });
}

export function viewImage(options) {
  return namedTool("view_image", {
    supportsParallelToolCalls: true,
    description: "View a local image file from the filesystem when visual inspection is needed. Use this for images already available on disk.",
    parameters: {
      type: "object",
      properties: {
        path: { type: "string", description: "Local filesystem path to an image file." },
        detail: { type: "string", enum: ["high", "original"], description: "Image detail level. Defaults to `high`; use `original` to preserve exact resolution." },
      },
      required: ["path"],
      additionalProperties: false,
    },
    outputSchema: {
      type: "object",
      properties: {
        image_url: { type: "string", description: "Data URL for the loaded image." },
        detail: { type: "string", enum: ["high", "original"], description: "Image detail hint returned by view_image. Returns `high` for default resized behavior or `original` when original resolution is preserved." },
      },
      required: ["image_url", "detail"],
      additionalProperties: false,
    },
    async handler(input) {
      const args = requireObject(input, "view_image");
      const path = requireString(args.path, "view_image.path");
      const detail = args.detail === undefined ? "high" : args.detail;
      if (detail !== "high" && detail !== "original") {
        throw new Error("view_image.detail must be high or original");
      }
      const bytes = await options.workspace.readFile(path);
      if (bytes.byteLength > MAX_VIEW_IMAGE_BYTES) {
        throw new Error("view_image input exceeds 10 MiB");
      }
      const mimeType = imageMimeType(bytes);
      if (!mimeType) throw new Error("view_image supports PNG, JPEG, GIF, and WebP files");
      const result = { detail, image_url: `data:${mimeType};base64,${base64(bytes)}` };
      return toolResult([{
        type: "input_image",
        image_url: result.image_url,
        detail,
      }], result, { value: result });
    },
  });
}

export function updatePlan() {
  const plans = new Map();
  return namedTool("update_plan", {
    description: "Update the current task plan. At most one step may be in progress.",
    parameters: {
      type: "object",
      properties: {
        explanation: { type: "string" },
        plan: {
          type: "array",
          items: {
            type: "object",
            properties: {
              step: { type: "string" },
              status: { type: "string", enum: ["pending", "in_progress", "completed"] },
            },
            required: ["step", "status"],
            additionalProperties: false,
          },
        },
      },
      required: ["plan"],
      additionalProperties: false,
    },
    outputSchema: {
      type: "object",
      properties: { updated: { type: "boolean", const: true } },
      required: ["updated"],
      additionalProperties: false,
    },
    async handler(input, context) {
      const value = requireObject(input, "update_plan");
      if (!Array.isArray(value.plan)) throw new Error("update_plan.plan must be an array");
      const active = value.plan.filter((item) =>
        typeof item === "object" && item !== null && item.status === "in_progress"
      );
      if (active.length > 1) throw new Error("at most one plan step may be in_progress");
      plans.set(context.sessionId, structuredClone(value));
      return { updated: true };
    },
    releaseSession(sessionId) {
      plans.delete(sessionId);
    },
    dispose() {
      plans.clear();
    },
  });
}

function jsonRequester(options, defaultUrl) {
  if (!options || typeof options !== "object" || Array.isArray(options)) {
    throw new TypeError("tool options must be an object");
  }
  const url = options.url ?? defaultUrl;
  if (typeof url !== "string" && !(url instanceof URL)) {
    throw new TypeError("tool url must be a string or URL");
  }
  const fetchImpl = options.fetch ?? globalThis.fetch;
  if (typeof fetchImpl !== "function") throw new TypeError("tool fetch must be a function");
  const headers = Object.freeze({
    "content-type": "application/json",
    ...options.headers,
  });
  return async (body, signal) => {
    const response = await fetchImpl(url, {
      method: "POST",
      headers,
      body: JSON.stringify(body),
      // These adapters commonly carry caller-owned bearer credentials.
      // Never let fetch forward them to a redirect target.
      redirect: "manual",
      signal,
    });
    if (response.type === "opaqueredirect" || (response.status >= 300 && response.status < 400)) {
      throw new Error("tool endpoint redirects are not allowed");
    }
    const payload = await response.json().catch(() => undefined);
    if (!response.ok) {
      const message = typeof payload?.error === "string" ? payload.error : `HTTP ${response.status}`;
      throw new Error(message);
    }
    return payload;
  };
}

function normalizeWebCommands(input) {
  const value = requireObject(input, "web__run");
  const commands = Object.keys(value).length === 1 && value.commands !== undefined
    ? requireObject(value.commands, "web__run.commands")
    : value;
  const normalized = { ...commands };
  for (const operation of [
    "search_query",
    "image_query",
    "open",
    "click",
    "find",
    "finance",
    "weather",
    "sports",
    "time",
  ]) {
    const command = normalized[operation];
    if (command === undefined || Array.isArray(command)) continue;
    if (typeof command === "object" && command !== null) {
      normalized[operation] = [command];
      continue;
    }
    if (typeof command === "string" && (operation === "search_query" || operation === "image_query")) {
      normalized[operation] = [{ q: command }];
      continue;
    }
    if (typeof command === "string" && operation === "open") {
      normalized.open = [{ ref_id: command }];
    }
  }
  const operationCount = [
    "search_query",
    "image_query",
    "open",
    "click",
    "find",
    "finance",
    "weather",
    "sports",
    "time",
  ].reduce((count, operation) => count + (Array.isArray(normalized[operation])
    ? normalized[operation].length
    : 0), 0);
  if (!operationCount) throw new Error("web__run requires at least one operation");
  const queryCount = Array.isArray(normalized.search_query) ? normalized.search_query.length : 0;
  if (queryCount > 4) throw new Error("web__run accepts at most 4 search queries");
  if (queryCount === 4 && normalized.response_length !== "medium" && normalized.response_length !== "long") {
    throw new Error("web__run requires response_length medium or long for 4 search queries");
  }
  return normalized;
}

function requireObject(value, name) {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new Error(`${name} requires an object`);
  }
  return value;
}

function requireString(value, name) {
  if (typeof value !== "string" || !value.trim()) throw new Error(`${name} must be a non-empty string`);
  return value;
}

function optionalInteger(value) {
  if (value === undefined || value === null) return undefined;
  if (!Number.isInteger(value)) throw new Error("num_last_images_to_include must be an integer");
  return value;
}

function optionalStringArray(value) {
  if (value === undefined || value === null) return [];
  if (!Array.isArray(value) || value.some((item) => typeof item !== "string" || !item.trim())) {
    throw new Error("referenced_image_paths must be an array of non-empty strings");
  }
  return value;
}

async function workspaceImage(workspace, path) {
  if (!workspace || typeof workspace.readFile !== "function") {
    throw new Error("referenced_image_paths requires an image-generation workspace");
  }
  const bytes = await workspace.readFile(path);
  if (bytes.byteLength > MAX_VIEW_IMAGE_BYTES) {
    throw new Error(`${path} exceeds the 10 MiB image limit`);
  }
  const mimeType = imageMimeType(bytes);
  if (!mimeType) throw new Error(`${path} is not a supported image`);
  return `data:${mimeType};base64,${base64(bytes)}`;
}

function imageMimeType(bytes) {
  if (startsWith(bytes, [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a])) return "image/png";
  if (startsWith(bytes, [0xff, 0xd8, 0xff])) return "image/jpeg";
  if (startsWith(bytes, [0x47, 0x49, 0x46, 0x38])) return "image/gif";
  if (startsWith(bytes, [0x52, 0x49, 0x46, 0x46])
    && startsWith(bytes.subarray(8), [0x57, 0x45, 0x42, 0x50])) return "image/webp";
  return undefined;
}

function startsWith(bytes, signature) {
  return signature.every((value, index) => bytes[index] === value);
}

function base64(bytes) {
  let binary = "";
  for (let offset = 0; offset < bytes.length; offset += 32_768) {
    binary += String.fromCharCode(...bytes.subarray(offset, offset + 32_768));
  }
  return btoa(binary);
}

const query = {
  description: "Search query with optional domain and recency filters.",
  type: "object",
  properties: {
    q: { type: "string" },
    recency: { type: "integer" },
    domains: { type: "array", items: { type: "string" } },
  },
  required: ["q"],
  additionalProperties: false,
};

const webParameters = {
  type: "object",
  properties: {
    search_query: {
      type: "array",
      description: "Query the internet search engine for a given list of queries.",
      maxItems: 4,
      items: query,
    },
    image_query: {
      type: "array",
      description: "Query the image search engine for source pages and captions.",
      items: query,
    },
    open: {
      type: "array",
      description: "Open pages by reference id or URL.",
      items: {
        type: "object",
        properties: { ref_id: { type: "string" }, lineno: { type: "integer" } },
        required: ["ref_id"],
        additionalProperties: false,
      },
    },
    click: {
      type: "array",
      description: "Open numbered links from previously opened pages.",
      items: {
        type: "object",
        properties: { ref_id: { type: "string" }, id: { type: "integer" } },
        required: ["ref_id", "id"],
        additionalProperties: false,
      },
    },
    find: {
      type: "array",
      description: "Find text patterns in pages.",
      items: {
        type: "object",
        properties: { ref_id: { type: "string" }, pattern: { type: "string" } },
        required: ["ref_id", "pattern"],
        additionalProperties: false,
      },
    },
    finance: {
      type: "array",
      description: "Look up prices for stock symbols and other assets.",
      items: {
        type: "object",
        properties: {
          ticker: { type: "string" },
          type: { type: "string", enum: ["equity", "fund", "crypto", "index"] },
          market: { type: "string" },
        },
        required: ["ticker", "type"],
        additionalProperties: false,
      },
    },
    weather: {
      type: "array",
      description: "Look up weather forecasts.",
      items: {
        type: "object",
        properties: {
          location: { type: "string" },
          start: { type: "string" },
          duration: { type: "integer" },
        },
        required: ["location"],
        additionalProperties: false,
      },
    },
    sports: {
      type: "array",
      description: "Look up sports schedules and standings.",
      items: {
        type: "object",
        properties: {
          fn: { type: "string", enum: ["schedule", "standings"] },
          league: {
            type: "string",
            enum: ["nba", "wnba", "nfl", "nhl", "mlb", "epl", "ncaamb", "ncaawb", "ipl"],
          },
          team: { type: "string" },
          opponent: { type: "string" },
          date_from: { type: "string" },
          date_to: { type: "string" },
          num_games: { type: "integer" },
          locale: { type: "string" },
        },
        required: ["fn", "league"],
        additionalProperties: false,
      },
    },
    time: {
      type: "array",
      description: "Get time for the given UTC offsets.",
      items: {
        type: "object",
        properties: { utc_offset: { type: "string" } },
        required: ["utc_offset"],
        additionalProperties: false,
      },
    },
    response_length: {
      type: "string",
      description: "Set the length of the returned response.",
      enum: ["short", "medium", "long"],
    },
  },
  additionalProperties: false,
};
