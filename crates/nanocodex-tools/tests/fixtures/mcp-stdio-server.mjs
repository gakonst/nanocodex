import readline from "node:readline";
import { spawn } from "node:child_process";

const lines = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });
const toolCount = Number.parseInt(
  process.env.NANOCODEX_MCP_FIXTURE_TOOL_COUNT ?? "1",
  10,
);
if (process.env.NANOCODEX_MCP_DESCENDANT_MARKER) {
  spawn(process.execPath, [
    "-e",
    "setTimeout(() => require('node:fs').writeFileSync(process.env.NANOCODEX_MCP_DESCENDANT_MARKER, 'survived'), 750)",
  ], {
    env: process.env,
    stdio: "ignore",
  });
}

function send(message) {
  process.stdout.write(`${JSON.stringify(message)}\n`);
}

lines.on("line", (line) => {
  const request = JSON.parse(line);
  if (request.method === "initialize") {
    send({
      jsonrpc: "2.0",
      id: request.id,
      result: {
        protocolVersion: request.params.protocolVersion,
        capabilities: { tools: {}, resources: {} },
        serverInfo: { name: "nanocodex-test-mcp", version: "0.1.0" },
      },
    });
  } else if (request.method === "tools/list") {
    const tools = Array.from({ length: toolCount }, (_, index) => {
      const suffix = index === 0 ? "" : `_${index}`;
      return {
        name: `echo${suffix}`,
        description: `Echo deterministic MCP fixture message ${index}.`,
        annotations: { readOnlyHint: true },
        inputSchema: {
          type: "object",
          properties: {
            message: { type: "string" },
            delay_ms: { type: "integer", minimum: 0, maximum: 1000 },
          },
          required: ["message"],
          additionalProperties: false,
        },
      };
    });
    send({
      jsonrpc: "2.0",
      id: request.id,
      result: { tools },
    });
  } else if (request.method === "tools/call") {
    const paid = request.params?._meta?.["org.paymentauth/credential"] === "fixture-paid";
    if (process.env.NANOCODEX_MCP_FIXTURE_PAYMENT && !paid) {
      if (process.env.NANOCODEX_MCP_FIXTURE_PAYMENT_RESULT) {
        send({
          jsonrpc: "2.0",
          id: request.id,
          result: {
            content: [],
            isError: true,
            _meta: {
              "org.paymentauth/payment-required": {
                challenges: [{ id: "fixture-payment", method: "tempo", intent: "charge" }],
              },
            },
          },
        });
        return;
      }
      send({
        jsonrpc: "2.0",
        id: request.id,
        error: {
          code: -32042,
          message: "Payment Required",
          data: {
            httpStatus: 402,
            challenges: [{ id: "fixture-payment", method: "tempo", intent: "charge" }],
          },
        },
      });
      return;
    }
    if (paid && process.env.NANOCODEX_MCP_FIXTURE_PAYMENT_REJECT) {
      send({
        jsonrpc: "2.0",
        id: request.id,
        error: { code: -32043, message: "Payment Rejected" },
      });
      return;
    }
    const message = request.params.arguments?.message;
    const failed = message === "__fail__";
    const text = failed ? "fixture:synthetic failure" : `fixture:${message}`;
    const delayMs = request.params.arguments?.delay_ms ?? 0;
    setTimeout(() => {
      send({
        jsonrpc: "2.0",
        id: request.id,
        result: {
          content: [{
            type: "text",
            text,
          }],
          structuredContent: { echoed: message, text },
          isError: failed,
        },
      });
    }, delayMs);
  } else if (request.method === "resources/list") {
    const secondPage = request.params?.cursor === "fixture-next";
    send({
      jsonrpc: "2.0",
      id: request.id,
      result: {
        resources: [{
          uri: secondPage ? "fixture://second" : "fixture://first",
          name: secondPage ? "fixture-second" : "fixture-first",
          mimeType: "text/plain",
        }],
        ...(secondPage ? {} : { nextCursor: "fixture-next" }),
      },
    });
  } else if (request.method === "resources/templates/list") {
    send({
      jsonrpc: "2.0",
      id: request.id,
      result: {
        resourceTemplates: [{
          uriTemplate: "fixture://item/{id}",
          name: "fixture-item",
          mimeType: "text/plain",
        }],
      },
    });
  } else if (request.method === "resources/read") {
    send({
      jsonrpc: "2.0",
      id: request.id,
      result: {
        contents: [{
          uri: request.params.uri,
          mimeType: "text/plain",
          text: "fixture resource body",
        }],
      },
    });
  }
});
