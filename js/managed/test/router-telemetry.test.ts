import { describe, expect, it } from "vitest";
import { DatabaseSync } from "node:sqlite";
import { RouterTelemetryStore, parseRouterObservation } from "../src/router-telemetry";
const now=1_000_000_000;
const sample={timestamp:now,clientIngressColo:"IAD",chosen:"openrouter:openai/gpt-5.6-luna:low",decision:"low",durationMs:50,
  classifier:{outcome:"success",attempts:[{duration_ms:50,outcome:"success"}]},confidence:.4,probabilities:null};
describe("content-free router telemetry",()=>{
  it("strips identities, text and nested errors and rejects unknown candidate labels",()=>{
    const projected=parseRouterObservation({...sample,prompt:"private",accountId:"private",classifier:{...sample.classifier,error:"private"}},now);
    expect(projected).toEqual(sample);expect(JSON.stringify(projected)).not.toContain("private");
    expect(parseRouterObservation({...sample,chosen:"private"},now)).toBeNull();
    expect(parseRouterObservation({...sample,probabilities:{"private":1}},now)).toBeNull();
    expect(parseRouterObservation({...sample,timestamp:now+1},now)).toBeNull();
    expect(parseRouterObservation({...sample,timestamp:now-7_200_001},now)).toBeNull();
  });
  it("bounds retained observations and excludes expired observations",()=>{
    const db=new DatabaseSync(":memory:");
    const store=new RouterTelemetryStore({exec(query:string,...args:any[]){const stmt=db.prepare(query);return stmt.columns().length ? stmt.all(...args) : (stmt.run(...args),[]);}});
    for(let i=0;i<520;i++)expect(store.append({...sample,timestamp:now+i},now+i)).toBe(true);
    expect(store.read(now+519)).toHaveLength(512);expect(store.read(now+7_200_520)).toHaveLength(0);db.close();
  });
});
