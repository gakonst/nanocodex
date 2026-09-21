// Protocol fixture only: it does not implement or describe a CUA JavaScript API.
export const png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+a7WQAAAAASUVORK5CYII=";
export const catalog = [
  { name: "js", description: "Synthetic MCP transport fixture", inputSchema: { type: "object", additionalProperties: true } },
  { name: "js_reset", description: "Synthetic provider reset", inputSchema: { type: "object", additionalProperties: true } },
];
export function provider({ blockMethod, requestLog } = {}) {
  const script = `
    let marker;
    const send = value => process.stdout.write(JSON.stringify({jsonrpc:'2.0',...value})+'\\n');
    require('node:readline').createInterface({input:process.stdin}).on('line', async line => {
      const request = JSON.parse(line);
      if (${JSON.stringify(requestLog)} !== undefined) require("node:fs").appendFileSync(${JSON.stringify(requestLog)}, request.method + "\\n");
      if (request.id === undefined || request.method === ${JSON.stringify(blockMethod)}) return;
      let result;
      if (request.method === 'initialize') result = {protocolVersion:'2025-06-18',capabilities:{tools:{}}};
      else if (request.method === 'tools/list') result = {tools:${JSON.stringify(catalog)}};
      else if (request.method === 'tools/call') {
        const args = request.params.arguments;
        if (request.params.name === 'js_reset') marker = undefined;
        if (args.set !== undefined) marker = args.set;
        if (args.crash) process.exit(0);
        if (args.block) return;
        if (args.wait) await new Promise(resolve => setTimeout(resolve, args.wait));
        result = {content: args.image ? [{type:'image',mimeType:'image/png',data:${JSON.stringify(png)}}]
          : [{type:'text',text:args.get ? String(marker) : args.large ? 'x'.repeat(args.large) : JSON.stringify(request.params)}],
          _meta:{provider:'fixture'}, ...(args.isError ? {isError:true} : {})};
      }
      send({id:request.id,result});
    });`;
  return { executable: process.execPath, args: ["-e", script] };
}
