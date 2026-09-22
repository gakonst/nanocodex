import { test } from "node:test";
import assert from "node:assert/strict";
import { routeProvider, summarizeRouter, type RouterDecision } from "./routerApi.ts";
import { pathForSurface, surfaceFromUrl } from "./navigation.ts";
test("router distinguishes failed attempts from recovered decisions and low confidence",()=>{
 const row={timestamp:1,clientIngressColo:null,chosen:"candidate",durationMs:10,confidence:null,probabilities:null};
 const rows:RouterDecision[]=[{...row,decision:"low",classifier:{outcome:"success",attempts:[{outcome:"unavailable",duration_ms:3},{outcome:"success",duration_ms:7}]}},
 {...row,decision:"not_requested",classifier:{outcome:"not_requested",attempts:[]}}];
 assert.deepEqual(summarizeRouter(rows),{decisions:2,attempts:2,bindingFailures:1,recovered:1,low:1,accepted:0,bypassed:1});
 assert.equal(pathForSurface("router"),"/router");assert.equal(surfaceFromUrl(new URL("https://example.com/router")),"router");
});

test("native candidate IDs still filter by the correct provider",()=>{
 assert.equal(routeProvider("@cf/zai-org/glm-5.3:low"),"workers_ai");
 assert.equal(routeProvider("gpt-6-astra:high"),"chatgpt");
 assert.equal(routeProvider("cloudflare:openai/gpt-6-astra:low"),"cloudflare");
});
