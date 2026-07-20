import readline from "node:readline";

const lines = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });

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
        capabilities: { tools: {} },
        serverInfo: { name: "nanocodex-test-mcp", version: "0.1.0" },
      },
    });
  } else if (request.method === "tools/list") {
    send({
      jsonrpc: "2.0",
      id: request.id,
      result: {
        tools: [{
          name: "echo",
          description: "Echo a message from the deterministic MCP fixture.",
          inputSchema: {
            type: "object",
            properties: { message: { type: "string" } },
            required: ["message"],
            additionalProperties: false,
          },
        }],
      },
    });
  } else if (request.method === "tools/call") {
    const message = request.params.arguments?.message;
    const failed = message === "__fail__";
    send({
      jsonrpc: "2.0",
      id: request.id,
      result: {
        content: [{
          type: "text",
          text: failed ? "fixture:synthetic failure" : `fixture:${message}`,
        }],
        structuredContent: { echoed: message },
        isError: failed,
      },
    });
  }
});
