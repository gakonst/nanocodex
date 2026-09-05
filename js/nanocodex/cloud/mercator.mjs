import { DEFAULT_MERCATOR_MCP_URL } from "../runtime/tempo-provider.mjs";

const MERCATOR_ORIGIN = new URL(DEFAULT_MERCATOR_MCP_URL).origin;
const JOBS_PATH = "/v1/jobs";
const MAX_BODY_BYTES = 64 * 1024;
const PAYMENT_DECIMALS = 6;

/** @internal Creates the bounded executor for Mercator's paid REST handoff. */
export function mercatorRestTool({ connection, fetch, calls, relay }) {
  if (typeof fetch !== "function") {
    throw new TypeError("Mercator REST execution requires an MPP-aware fetch function");
  }
  if (!connection.mpp) {
    throw new TypeError("Mercator requires a Connect grant with explicit payment authority");
  }
  const relayUrl = new URL(relay);
  return {
    description: "Execute Mercator's run_rest_request handoff through MPP. Use only the exact rest and maxSpend fields returned by Mercator create_job, then poll the returned job with Mercator get_job.",
    parameters: {
      type: "object",
      properties: {
        url: { type: "string" },
        method: { type: "string", enum: ["POST"] },
        body: { type: "object", additionalProperties: true },
        maxSpend: { type: "string", description: "Maximum quoted spend in decimal USDC, copied from the Mercator handoff." },
      },
      required: ["url", "method", "body", "maxSpend"],
      additionalProperties: false,
    },
    async handler(input, context) {
      calls.add("run_rest_request", context);
      const url = mercatorJobUrl(input?.url);
      if (input?.method !== "POST") {
        throw new Error("Mercator REST handoffs must use POST.");
      }
      const maxAmount = decimalAtomics(input?.maxSpend, PAYMENT_DECIMALS);
      if (maxAmount <= 0n) throw new Error("Mercator maxSpend must be greater than zero.");
      if (maxAmount > connection.mpp.maxPerRequest) {
        throw new Error(
          `Mercator maxSpend exceeds this Connect grant's per-request limit (${connection.mpp.maxPerRequest} atomics).`,
        );
      }
      if (!plainObject(input.body)) {
        throw new TypeError("Mercator REST handoff body must be an object.");
      }
      const body = JSON.stringify(input.body);
      if (new TextEncoder().encode(body).byteLength > MAX_BODY_BYTES) {
        throw new Error(`Mercator REST handoff body exceeds ${MAX_BODY_BYTES} bytes.`);
      }

      const response = await fetch(relayUrl, {
        method: "POST",
        headers: { accept: "application/json", "content-type": "application/json" },
        body,
      }, { intent: "charge", maxAmount });
      const responseText = await response.text();
      if (!response.ok) {
        throw new Error(
          `Mercator job submission failed with HTTP ${response.status}: ${responseText.slice(0, 2_048)}`,
        );
      }
      return {
        status: response.status,
        paymentReceipt: response.headers.get("payment-receipt") ?? undefined,
        result: parseResponse(responseText),
      };
    },
  };
}

function mercatorJobUrl(value) {
  let url;
  try {
    url = new URL(value);
  } catch (error) {
    throw new TypeError("Mercator REST handoff URL is invalid.", { cause: error });
  }
  if (url.origin !== MERCATOR_ORIGIN || url.pathname !== JOBS_PATH
    || url.username || url.password || url.search || url.hash) {
    throw new Error(`Mercator REST handoff must target ${MERCATOR_ORIGIN}${JOBS_PATH}.`);
  }
  return url.href;
}

function decimalAtomics(value, decimals) {
  if (typeof value !== "string") throw new TypeError("Mercator maxSpend must be a decimal string.");
  const match = /^(\d+)(?:\.(\d+))?$/.exec(value);
  if (!match || (match[2]?.length ?? 0) > decimals) {
    throw new TypeError(`Mercator maxSpend must have at most ${decimals} decimal places.`);
  }
  return BigInt(match[1]) * (10n ** BigInt(decimals))
    + BigInt((match[2] ?? "").padEnd(decimals, "0") || "0");
}

function plainObject(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const prototype = Object.getPrototypeOf(value);
  return prototype === Object.prototype || prototype === null;
}

function parseResponse(value) {
  if (!value) return null;
  try {
    return JSON.parse(value);
  } catch {
    return value;
  }
}
