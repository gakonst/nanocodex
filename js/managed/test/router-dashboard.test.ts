import { describe, expect, it, vi } from "vitest";
import { createExecutionContext, env } from "cloudflare:test";
import worker from "../src/index";
import { routerDashboard } from "../src/router-dashboard";
import type { Principal } from "../src/account-auth";
const principal = {kind:"account_session",userId:"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",role:"owner"} as Principal;
const request=(method="GET",suffix="")=>new Request("https://example.com/v1/router"+suffix,{method});
const fixture=()=>({NANOCODEX_ADMIN_USER_ID:principal.userId,NANOCODEX_PROVIDER_PROBE_COORDINATOR:{getByName:vi.fn(()=>({dashboardSnapshot:vi.fn(async()=>({version:1,providers:[],decisions:[]}))}))}});
describe("router dashboard authorization",()=>{
  it("gates the actual Worker route",async()=>{
    const config=fixture();
    const call=(actor?:Principal)=>worker.fetch(request(),{...env,...config} as unknown as Parameters<typeof worker.fetch>[1],createExecutionContext(),actor);
    expect((await call()).status).toBe(401); expect((await call({...principal,userId:"other"})).status).toBe(403);
    expect((await call(principal)).status).toBe(200);
  });
  it("denies API keys, Connect grants, other users and unset admin without touching telemetry",async()=>{
    const config=fixture();
    for(const actor of [{...principal,kind:"api_key" as const},{...principal,userId:"other"},{...principal,connectGrant:{grantId:"grant"} as NonNullable<Principal["connectGrant"]>}]){
      expect((await routerDashboard(request(),config,actor)).status).toBe(403);
    }
    expect((await routerDashboard(request(),{...config,NANOCODEX_ADMIN_USER_ID:undefined},principal)).status).toBe(403);
    expect(config.NANOCODEX_PROVIDER_PROBE_COORDINATOR.getByName).not.toHaveBeenCalled();
  });
  it("allows read-only no-selector access and never caches",async()=>{
    const config=fixture();
    expect((await routerDashboard(request("POST"),config,principal)).status).toBe(405);
    expect((await routerDashboard(request("GET","?user=other"),config,principal)).status).toBe(400);
    const response=await routerDashboard(request(),config,principal);
    expect(response.headers.get("cache-control")).toBe("no-store"); expect(await response.json()).toEqual({version:1,providers:[],decisions:[]});
  });
  it("reports unavailable telemetry without fabricating an empty success",async()=>{
    expect((await routerDashboard(request(),{NANOCODEX_ADMIN_USER_ID:principal.userId},principal)).status).toBe(503);
  });
});

it("real coordinator persists content-free decisions and returns separate ingress/global cohorts",async()=>{
 const binding=(env as unknown as {NANOCODEX_PROVIDER_PROBE_COORDINATOR:{getByName(name:string):any}}).NANOCODEX_PROVIDER_PROBE_COORDINATOR;
 const stub=binding.getByName("dashboard-test-"+crypto.randomUUID());const now=Date.now();
 expect(await stub.observeRoute({timestamp:now,clientIngressColo:"IAD",chosen:"openrouter:openai/gpt-5.6-luna:low",decision:"not_requested",durationMs:0,
   classifier:{outcome:"not_requested",attempts:[]},confidence:null,probabilities:null,prompt:"never persist"})).toBe(true);
 expect(await stub.observe({timestamp:now,source:"live",workerColo:null,clientIngressColo:"IAD",backend:"openrouter",model:"gpt-5.6-luna",effort:"low",
   outcome:"success",status:200,headersMs:5,fullResponseMs:30,generationTtftMs:10,clientDeliveryMs:null,elapsedMs:30})).toBe(true);
 const snapshot=await stub.dashboardSnapshot();expect(snapshot.version).toBe(1);
 expect(snapshot.decisions).toHaveLength(1);expect(JSON.stringify(snapshot)).not.toContain("never persist");
 expect(snapshot.providers.map((p:any)=>p.scope).sort()).toEqual(["client_ingress","deployment_global"]);
 expect(snapshot.providers.every((p:any)=>p.sampleCount===1)).toBe(true);
});
