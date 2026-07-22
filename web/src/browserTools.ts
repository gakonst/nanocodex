type ToolContext = {
  sessionId: string;
};

type ToolDefinition = {
  description: string;
  parameters: Record<string, unknown>;
  handler(input: unknown, context: ToolContext): Promise<unknown>;
};

type BrowserToolOptions = {
  recentImages(sessionId: string, count: number): string[];
  rememberImage(sessionId: string, imageUrl: string): void;
};

const WEB_DESCRIPTION = `Search and inspect the public internet. Use search_query for web
search, image_query for image-source discovery, open/click/find for pages, and the specialized
finance, weather, sports, and time operations when applicable. Ref IDs returned by one call can
be used by later calls in the same session.`;

const IMAGE_DESCRIPTION = `Generate a new image or edit recently attached/generated conversation
images. For a new image, provide only prompt. For an edit, provide num_last_images_to_include from
1 to 5. The result must be passed to generatedImage(result) so the image enters the conversation.`;

export function createBrowserTools(options: BrowserToolOptions): Record<string, ToolDefinition> {
  const plans = new Map<string, unknown>();
  return {
    web__run: {
      description: WEB_DESCRIPTION,
      parameters: webParameters,
      async handler(input, context) {
        const commands = requireObject(input, "web__run");
        const result = await postJson<{ output: string }>("/api/tools/web-search", {
          commands,
          session_id: context.sessionId,
        });
        return result.output;
      },
    },
    image_gen__imagegen: {
      description: IMAGE_DESCRIPTION,
      parameters: {
        type: "object",
        properties: {
          prompt: { type: "string" },
          num_last_images_to_include: {
            type: "integer",
            minimum: 1,
            maximum: 5,
          },
        },
        required: ["prompt"],
        additionalProperties: false,
      },
      async handler(input, context) {
        const args = requireObject(input, "image_gen__imagegen");
        const prompt = requireString(args.prompt, "image_gen__imagegen.prompt");
        const count = optionalInteger(args.num_last_images_to_include);
        if (count !== undefined && (count < 1 || count > 5)) {
          throw new Error("num_last_images_to_include must be between 1 and 5");
        }
        const images = count === undefined ? [] : options.recentImages(context.sessionId, count);
        if (count !== undefined && images.length !== count) {
          throw new Error(`requested ${count} recent images, but only ${images.length} are available`);
        }
        const result = await postJson<{ image_url: string }>("/api/tools/image-generation", {
          images,
          prompt,
        });
        options.rememberImage(context.sessionId, result.image_url);
        return result;
      },
    },
    update_plan: {
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
                status: {
                  type: "string",
                  enum: ["pending", "in_progress", "completed"],
                },
              },
              required: ["step", "status"],
              additionalProperties: false,
            },
          },
        },
        required: ["plan"],
        additionalProperties: false,
      },
      async handler(input, context) {
        const value = requireObject(input, "update_plan");
        if (!Array.isArray(value.plan)) throw new Error("update_plan.plan must be an array");
        const active = value.plan.filter((item) =>
          typeof item === "object" && item !== null && (item as { status?: unknown }).status === "in_progress"
        );
        if (active.length > 1) throw new Error("at most one plan step may be in_progress");
        plans.set(context.sessionId, structuredClone(value));
        return { updated: true };
      },
    },
  };
}

async function postJson<T>(path: string, body: unknown): Promise<T> {
  const response = await fetch(path, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
  const payload = await response.json().catch(() => undefined) as { error?: unknown } | undefined;
  if (!response.ok) {
    const message = typeof payload?.error === "string" ? payload.error : `HTTP ${response.status}`;
    throw new Error(message);
  }
  return payload as T;
}

function requireObject(value: unknown, name: string): Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new Error(`${name} requires an object`);
  }
  return value as Record<string, unknown>;
}

function requireString(value: unknown, name: string): string {
  if (typeof value !== "string" || !value.trim()) throw new Error(`${name} must be a non-empty string`);
  return value;
}

function optionalInteger(value: unknown): number | undefined {
  if (value === undefined) return undefined;
  if (!Number.isInteger(value)) throw new Error("num_last_images_to_include must be an integer");
  return value as number;
}

const query = {
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
    search_query: { type: "array", maxItems: 4, items: query },
    image_query: { type: "array", items: query },
    open: {
      type: "array",
      items: {
        type: "object",
        properties: { ref_id: { type: "string" }, lineno: { type: "integer" } },
        required: ["ref_id"],
        additionalProperties: false,
      },
    },
    click: {
      type: "array",
      items: {
        type: "object",
        properties: { ref_id: { type: "string" }, id: { type: "integer" } },
        required: ["ref_id", "id"],
        additionalProperties: false,
      },
    },
    find: {
      type: "array",
      items: {
        type: "object",
        properties: { ref_id: { type: "string" }, pattern: { type: "string" } },
        required: ["ref_id", "pattern"],
        additionalProperties: false,
      },
    },
    finance: {
      type: "array",
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
      items: {
        type: "object",
        properties: { utc_offset: { type: "string" } },
        required: ["utc_offset"],
        additionalProperties: false,
      },
    },
    response_length: { type: "string", enum: ["short", "medium", "long"] },
  },
  additionalProperties: false,
};
