const endpoint = process.env.NANOCODEX_WORKER_URL ?? "http://127.0.0.1:8787";
const token = process.env.NANOCODEX_API_TOKEN ?? "local-demo-token";
const prompt = process.argv.slice(2).join(" ")
  || "Use tool_search to find Mercator service discovery. Find three Tempo data services and summarize them.";

const response = await fetch(new URL("/v1/fetch", endpoint), {
  method: "POST",
  headers: {
    authorization: `Bearer ${token}`,
    "content-type": "application/json",
  },
  body: JSON.stringify({ prompt, thinking: "high" }),
});

const body = await response.json();
if (!response.ok) throw new Error(`${response.status}: ${JSON.stringify(body)}`);
console.log(body.final_message);
console.error(JSON.stringify({ payments: body.payments, usage: body.usage }, null, 2));
