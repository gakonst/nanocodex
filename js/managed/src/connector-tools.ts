import type { ToolContext } from "nanocodex";
import type { ConnectorCapabilityId } from "./connector-status";

/** Discovery metadata only; live grants and credentials are enforced by managed egress. */
export const CONNECTOR_TOOL_CATALOG = {
  link: {
    methods: ["GET", "POST"],
    docs: "https://github.com/stripe/link-cli#spend-request-lifecycle",
    operations: "POST /spend_requests with {merchant_name,merchant_url,context,amount,currency} creates a request; amount is in minor currency units and context must explain the purchase (at least 100 characters). POST /spend_requests/ID/request_approval sends the user a Link approval notification and returns approval_link; show it to the user. GET /spend_requests/ID checks status; POST /spend_requests/ID/cancel cancels. GET /userinfo reads wallet limits. Use test:true for test requests. Never retry a write after an ambiguous failure; list GET /spend_requests to reconcile. Approval happens in Link; this connector cannot approve spends or retrieve payment credentials.",
    origin: "https://api.link.com",
    summary: "Stripe Link wallet: create spend requests, request user approval for purchases, check approval status and cancel requests.",
    example: "/spend_requests",
  },
  github: {
    methods: ["DELETE", "GET", "PATCH", "POST", "PUT"],
    docs: "https://docs.github.com/en/rest",
    operations: "GET /user; GET /repos/OWNER/REPO/issues; POST /repos/OWNER/REPO/issues with {title,body}; PATCH /repos/OWNER/REPO/issues/NUMBER with {state}.",
    origin: "https://api.github.com",
    summary: "GitHub repositories, issues, pull requests, code search, commits, releases and comments.",
    example: "/user/repos?per_page=20",
  },
  gmail: {
    methods: ["DELETE", "GET", "PATCH", "POST", "PUT"],
    docs: "https://developers.google.com/workspace/gmail/api/reference/rest",
    operations: "GET /gmail/v1/users/me/messages?q=QUERY; GET /gmail/v1/users/me/messages/MESSAGE_ID; POST /gmail/v1/users/me/messages/send with {raw: base64url-encoded MIME}.",
    origin: "https://gmail.googleapis.com",
    summary: "Gmail search, read messages and threads, labels, drafts and send email.",
    example: "/gmail/v1/users/me/messages?maxResults=20",
  },
  gdrive: {
    methods: ["DELETE", "GET", "PATCH", "POST"],
    docs: "https://developers.google.com/workspace/drive/api/reference/rest/v3",
    operations: "GET /drive/v3/files?q=QUERY; POST /drive/v3/files with {name,mimeType}; PATCH /drive/v3/files/FILE_ID with {name}; DELETE /drive/v3/files/FILE_ID. JSON metadata only; this tool does not upload binary file content.",
    origin: "https://www.googleapis.com",
    summary: "Google Drive search files and folders, read metadata, create, update, share and delete files.",
    example: "/drive/v3/files?pageSize=20",
  },
  gcalendar: {
    methods: ["DELETE", "GET", "PATCH", "POST", "PUT"],
    docs: "https://developers.google.com/workspace/calendar/api/v3/reference",
    operations: "GET /calendar/v3/calendars/primary/events; POST /calendar/v3/calendars/primary/events with {summary,start,end}; PATCH /calendar/v3/calendars/primary/events/EVENT_ID; DELETE the same event path.",
    origin: "https://www.googleapis.com",
    summary: "Google Calendar list calendars, read, create, update and delete events, availability and free busy.",
    example: "/calendar/v3/calendars/primary/events?maxResults=20",
  },
  gtasks: {
    methods: ["DELETE", "GET", "PATCH", "POST", "PUT"],
    docs: "https://developers.google.com/workspace/tasks/reference/rest",
    operations: "GET /tasks/v1/users/@me/lists; GET or POST /tasks/v1/lists/TASKLIST_ID/tasks; PATCH /tasks/v1/lists/TASKLIST_ID/tasks/TASK_ID with {status: completed}; DELETE the same task path.",
    origin: "https://tasks.googleapis.com",
    summary: "Google Tasks list task lists, read, create, update, complete and delete tasks.",
    example: "/tasks/v1/users/@me/lists",
  },
  gdocs: {
    methods: ["GET", "POST"],
    docs: "https://developers.google.com/workspace/docs/api/reference/rest",
    operations: "GET /v1/documents/DOCUMENT_ID; POST /v1/documents/DOCUMENT_ID:batchUpdate with {requests:[...]}.",
    origin: "https://docs.googleapis.com",
    summary: "Google Docs read documents and edit document content with batchUpdate.",
    example: "/v1/documents/DOCUMENT_ID",
  },
  gsheets: {
    methods: ["GET", "POST", "PUT"],
    docs: "https://developers.google.com/workspace/sheets/api/reference/rest",
    operations: "GET /v4/spreadsheets/SPREADSHEET_ID; GET or PUT /v4/spreadsheets/SPREADSHEET_ID/values/RANGE (PUT requires valueInputOption query and {values:[...]} body); POST /v4/spreadsheets/SPREADSHEET_ID:batchUpdate with {requests:[...]}.",
    origin: "https://sheets.googleapis.com",
    summary: "Google Sheets read spreadsheets and cell ranges, write values, append rows and batchUpdate formatting.",
    example: "/v4/spreadsheets/SPREADSHEET_ID",
  },
  gslides: {
    methods: ["GET", "POST"],
    docs: "https://developers.google.com/workspace/slides/api/reference/rest",
    operations: "GET /v1/presentations/PRESENTATION_ID; POST /v1/presentations/PRESENTATION_ID:batchUpdate with {requests:[...]}.",
    origin: "https://slides.googleapis.com",
    summary: "Google Slides read presentations and edit slides, text, shapes and layout with batchUpdate.",
    example: "/v1/presentations/PRESENTATION_ID",
  },
  gcontacts: {
    methods: ["GET"],
    docs: "https://developers.google.com/people/api/rest",
    operations: "GET /v1/people/me/connections?personFields=names,emailAddresses; GET /v1/contactGroups. The connector requests contacts.readonly, so this tool supports reads only.",
    origin: "https://people.googleapis.com",
    summary: "Google Contacts People API search and read contacts and contact groups (read-only grant).",
    example: "/v1/people/me/connections?personFields=names,emailAddresses",
  },
  slack: {
    methods: ["GET", "POST"],
    docs: "https://docs.slack.dev/reference/methods/",
    operations: "GET /api/conversations.list; GET /api/conversations.history?channel=CHANNEL_ID; POST /api/chat.postMessage with {channel,text}; POST /api/chat.update with {channel,ts,text}; POST /api/reactions.add with {channel,timestamp,name}. Slack failures may be HTTP 200 with data.ok=false.",
    origin: "https://slack.com",
    summary: "Slack search messages, read conversations and threads, channels, users, send and update messages, reactions.",
    example: "/api/conversations.list?limit=20",
  },
  x: {
    methods: ["DELETE", "GET", "POST", "PUT"],
    docs: "https://api.x.com/2/openapi.json",
    operations: "GET /2/users/me; POST /2/tweets with {text}; DELETE /2/tweets/POST_ID; POST /2/users/USER_ID/likes with {tweet_id}; POST /2/users/USER_ID/bookmarks with {tweet_id}. Account tier, scopes and provider usage limits apply.",
    origin: "https://api.x.com",
    summary: "X Twitter account profile, tweets, posts, likes, bookmarks, follows, lists and direct messages.",
    example: "/2/users/me",
  },
  spotify: {
    methods: ["GET", "POST", "PUT", "DELETE"],
    docs: "https://developer.spotify.com/documentation/web-api/reference",
    operations: "GET /v1/me/player/recently-played?limit=20 (user-read-recently-played scope; limit 1..50; before/after are Unix milliseconds and mutually exclusive). GET /v1/me/playlists; POST /v1/me/playlists with {name,public:false}; GET /v1/playlists/PLAYLIST_ID/items; POST the items path with {uris:[spotify:track:ID]}; DELETE it with {items:[{uri:spotify:track:ID}]}; PUT /v1/playlists/PLAYLIST_ID with {name,description}. GET /v1/me/tracks; GET /v1/me/following?type=artist; GET /v1/search?q=QUERY&type=track; GET /v1/me/player; PUT /v1/me/player/play or /v1/me/player/pause. 204 playback responses mean no content; 403 can mean missing scopes/account eligibility; 429 means wait retry_after. Do not invent a recently_played method: use GET and the exact recently-played path.",
    origin: "https://api.spotify.com",
    summary: "Spotify music search, read playlists and tracks, create and edit playlists, add or remove songs, saved library, followed artists, listening history and playback. GET /v1/me/playlists lists your playlists; POST /v1/me/playlists creates one; GET or POST /v1/playlists/PLAYLIST_ID/items reads or adds tracks. Playback needs an eligible account and device.",
    example: "/v1/me/playlists?limit=20",
  },
  soundcloud: {
    methods: ["GET", "POST", "PUT", "DELETE"],
    docs: "https://developers.soundcloud.com/docs/api/explorer/api.json",
    operations: "GET /me/playlists; GET /tracks?q=QUERY; POST /playlists with {playlist:{title,tracks:[{urn:TRACK_URN}]}}; PUT /playlists/PLAYLIST_URN with {playlist:{title}}; POST or DELETE /likes/tracks/TRACK_URN. Use SoundCloud URNs returned by the API, URL-encoded as path components. This JSON tool does not upload audio files.",
    origin: "https://api.soundcloud.com",
    summary: "SoundCloud search tracks and users, read and edit playlists, likes, reposts and account profile.",
    example: "/me/playlists?limit=20",
  },
} as const satisfies Record<ConnectorCapabilityId, { origin: string; summary: string; example: string; methods: readonly string[]; docs: string; operations: string }>;

export function connectorToolMetadata(capabilities: readonly ConnectorCapabilityId[]) {
  return Object.fromEntries(capabilities.map(id => [id, {
    tool: `${id}_request`,
    description: CONNECTOR_TOOL_CATALOG[id].summary,
    documentation: CONNECTOR_TOOL_CATALOG[id].docs,
  }]));
}

const PARAMETERS = {
  type: "object",
  properties: {
    method: { type: "string", enum: ["GET", "POST", "PUT", "PATCH", "DELETE"], description: "HTTP method; defaults to GET. Writes require the user's requested action." },
    path: { type: "string", maxLength: 8192, description: "Absolute API path with optional query string, starting with one /. No host, credentials or fragment. Use provider pagination with bounded page sizes." },
    connection_id: { type: "string", pattern: "^[A-Za-z0-9_-]{43}$", description: "Exact accounts[service].connections id from environment(). Required to select among multiple accounts." },
    body: { type: "object", additionalProperties: true, description: "JSON request body for a write operation, using the provider API schema." },
  },
  required: ["path"],
  additionalProperties: false,
};

type Options = {
  available(capability: ConnectorCapabilityId): boolean;
  fetch(request: Request, context: ToolContext, capability: ConnectorCapabilityId): Promise<Response>;
};

/** First-party deferred tools share tool_search with connected MCPs and hands. */
export function connectorToolsProvider(options: Options) {
  const tools = Object.entries(CONNECTOR_TOOL_CATALOG).map(([key, spec]) => {
    const capability = key as ConnectorCapabilityId;
    const name = `${capability}_request`;
    return {
      capability,
      definition: {
        type: "function" as const, name, defer_loading: true as const, strict: false,
        description: `${spec.summary} Authenticated JSON HTTP request tool at ${spec.origin}. Use method and path, not invented operation names. Example GET ${spec.example}. ${spec.operations} Other paths and bodies must follow ${spec.docs}. Requires a connected account and granted provider scopes; inspect environment().accounts for current availability. Credentials and token refresh stay in the broker. Never automatically retry writes. On 429 respect retry_after.`,
        parameters: { ...PARAMETERS, properties: { ...PARAMETERS.properties, method: { ...PARAMETERS.properties.method, enum: [...spec.methods] } } },
      },
      tool: {
        name, parallelSafe: false,
        handler: async (input: unknown, context: ToolContext) => {
          if (!options.available(capability)) throw new Error("Connector tool unavailable for this grant");
          const request = connectorRequest(spec.origin, spec.methods, input, context.signal);
          const response = await options.fetch(request, context, capability);
          const reader = response.body?.getReader();
          let text = "";
          if (reader) {
            const decoder = new TextDecoder();
            let bytes = 0;
            try {
              while (true) {
                const { done, value } = await reader.read();
                if (done) break;
                bytes += value.byteLength;
                if (bytes > 512 * 1024) {
                  await reader.cancel();
                  return { ok: response.ok, status: response.status, response_too_large: true,
                    message: "The request completed, but its response exceeded 512 KiB. Read with a smaller page size or field selection. Do not repeat a write." };
                }
                text += decoder.decode(value, { stream: true });
              }
              text += decoder.decode();
            } finally { reader.releaseLock(); }
          }
          let data: unknown = null;
          if (text) { try { data = JSON.parse(text); } catch { data = text; } }
          return { ok: response.ok && !(capability === "slack" && typeof data === "object" && data !== null && "ok" in data && data.ok === false), status: response.status,
            ...(response.headers.has("retry-after") ? { retry_after: response.headers.get("retry-after") } : {}),
            ...(response.headers.has("link") ? { link: response.headers.get("link") } : {}), data };
        },
      },
    };
  });
  return {
    sourceId: "account-connectors",
    definitions: () => tools.filter(t => options.available(t.capability)).map(t => t.definition),
    resolve: (name: string) => tools.find(t => t.tool.name === name && options.available(t.capability))?.tool,
  };
}

function connectorRequest(origin: string, methods: readonly string[], input: unknown, signal: AbortSignal): Request {
  if (!input || typeof input !== "object" || Array.isArray(input)) throw new TypeError("Expected request object");
  const value = input as Record<string, unknown>;
  if (Object.keys(value).some(k => !["method", "path", "connection_id", "body"].includes(k))) throw new TypeError("Unknown request field");
  const method = value.method ?? "GET";
  if (typeof method !== "string" || !methods.includes(method)) throw new TypeError("Invalid method");
  if (typeof value.path !== "string" || value.path.length > 8192 || !/^\/(?!\/)/.test(value.path)
    || /[\\#\u0000-\u0020\u007f]/.test(value.path)) throw new TypeError("Invalid API path");
  const url = new URL(value.path, origin);
  if (url.origin !== origin || url.username || url.password || url.hash) throw new TypeError("Invalid API destination");
  const headers = new Headers({ accept: "application/json" });
  if (value.connection_id !== undefined) {
    if (typeof value.connection_id !== "string" || !/^[A-Za-z0-9_-]{43}$/.test(value.connection_id)) throw new TypeError("Invalid connection_id");
    headers.set("x-nanocodex-connector-connection", value.connection_id);
  }
  let body: string | undefined;
  if (value.body !== undefined) {
    if (method === "GET" || !value.body || typeof value.body !== "object" || Array.isArray(value.body)) throw new TypeError("Invalid JSON body");
    body = JSON.stringify(value.body);
    if (new TextEncoder().encode(body).byteLength > 256 * 1024) throw new TypeError("Request body exceeds 256 KiB");
    headers.set("content-type", "application/json");
  }
  return new Request(url, { method, headers, body, redirect: "manual",
    signal: AbortSignal.any([signal, AbortSignal.timeout(30_000)]) });
}
