import { URL as NodeURL } from "node:url";
import { readFileSync } from "node:fs";
const preferenceObservations = JSON.parse(readFileSync(new NodeURL("./fixtures/thread-routing-preferences-20260921.json", import.meta.url), "utf8")) as {
  cases: Array<{id:string; catalog:number; prompt:string; preferences:Record<string,unknown>;
    answers:{candidate:{confidence:number}}; expected_choice:string; expected_selection:string}>;
};
import { describe, expect, it, vi } from "vitest";
import { DatabaseSync } from "node:sqlite";
import { resolveThreadRoute, routingPolicySchema, ThreadRoutePin, ROUTING_CANDIDATES, OSS_MODEL, FRONTIER_MODEL, projectThreadRouteDiagnostics, taskFamily } from "../src/thread-model-routing";
import { initializeManagedAgentSettingsSchema } from "../src/agent-settings-schema";
import { parseAgentCreateBody, validateAgentSettings } from "../src/agent-settings";
import { parseConfiguration } from "../src/agent-configuration";

const policy = (patch = {}) => routingPolicySchema.parse({ strategy: "legacy", ...patch });
const jev = (family = "terminal", confidence = .98) => ({ run: vi.fn(async (_model: string, _input: unknown) => ({ answers: { family: { choice: family, confidence } }, usage: { input_tokens: 70 } })) });

describe("eval-informed thread routing", () => {
  it("uses Jev's typed classifier and pins actual OSS identity/effort", async () => {
    const ai = jev(); const route = await resolveThreadRoute(ai, "Fix my build", policy({ oss_thinking: "high" }));
    expect(ai.run.mock.calls[0][0]).toBe("typesafe/jev");
    expect(route).toMatchObject({ backend: "workers_ai", model: OSS_MODEL, thinking: "high", selection: "prior", estimate: null });
    expect(route.evidence.eval).toBe("Terminal-Bench 2.1");
  });
  it("does not claim published scores are local completion probabilities", async () => {
    const route = await resolveThreadRoute(jev("long_engineering"), "Add feature", policy());
    expect(route.evidence).toHaveProperty("oss_score", 66.9);
    expect(route.estimate).toBeNull();
    expect(route.selection).toBe("prior");
  });
  it.each(["other", "desktop", "science", "research", "mathematics"])("uses frontier for %s without comparable local measurements", async family => {
    expect((await resolveThreadRoute(jev(family), "task", policy())).model).toBe(FRONTIER_MODEL);
  });
  it("keeps low-confidence or invalid classifier output on explicit fallback", async () => {
    expect((await resolveThreadRoute(jev("terminal", .4), "task", policy())).selection).toBe("fallback");
    expect((await resolveThreadRoute(jev("made_up_eval"), "task", policy())).selection).toBe("fallback");
  });
  it("pins fallback on Jev error without exposing provider errors", async () => {
    const route = await resolveThreadRoute({ run: async () => { throw new Error("secret provider text"); } }, "task", policy());
    expect(route.model).toBe(FRONTIER_MODEL);
    expect(JSON.stringify(route)).not.toContain("secret provider text");
  });
  it("does not send binary modalities or oversized state to Jev", async () => {
    const ai = jev();
    const image = await resolveThreadRoute(ai, [{ type: "image", image_url: "data:image/png;base64,abc" }], policy());
    expect(image.backend).toBe("chatgpt");
    expect((await resolveThreadRoute(ai, "x".repeat(24001), policy())).selection).toBe("fallback");
    expect(ai.run).not.toHaveBeenCalled();
  });
  const estimates = [
    { family: "terminal", backend: "workers_ai", model: OSS_MODEL, thinking: "medium", success_rate: .5, expected_cost_usd: .1, expected_duration_ms: 5000, sample_size: 100, source: "heldout-v1" },
    { family: "terminal", backend: "chatgpt", model: FRONTIER_MODEL, thinking: "high", success_rate: .9, expected_cost_usd: .3, expected_duration_ms: 6000, sample_size: 100, source: "heldout-v1" },
  ];
  it("chooses amortized cost/success and duration/success using matched measurements", async () => {
    const cost = await resolveThreadRoute(jev(), "task", policy({ objective: "cost", estimates }));
    const time = await resolveThreadRoute(jev(), "task", policy({ objective: "time", estimates }));
    expect(cost.backend).toBe("workers_ai"); // .20 vs .333 USD/accepted completion
    expect(time.backend).toBe("chatgpt"); // 6.67 vs 10 sec/accepted completion
    expect(cost.selection).toBe("measured");
    expect((await resolveThreadRoute(jev(), "task", policy({ objective: "effectiveness", estimates }))).backend).toBe("chatgpt");
  });
  it("never restores a route excluded by the success threshold", async () => {
    const route = await resolveThreadRoute(jev(), "task", policy({ estimates, objective: "cost", min_success_rate: .8 }));
    expect(route.backend).toBe("chatgpt"); expect(route.selection).toBe("measured");
    await expect(resolveThreadRoute(jev(), "task", policy({ estimates, min_success_rate: .95 }))).rejects.toThrow("no route admitted");
    await expect(resolveThreadRoute(jev(), "task", policy({ min_success_rate: .8 }))).rejects.toThrow("no route admitted");
    expect(() => policy({ estimates: [...estimates, estimates[0]] })).toThrow();
  });
  it("supports configured ChatGPT model and refuses mixed measurement sources", async () => {
    expect((await resolveThreadRoute(jev("research"), "task", policy({ frontier_model: "gpt-5.6-sol", frontier_thinking: "low" }))).model).toBe("gpt-5.6-sol");
    const mixed = estimates.map((e, i) => ({ ...e, source: `dataset-${i}` }));
    expect((await resolveThreadRoute(jev(), "task", policy({ estimates: mixed }))).selection).toBe("prior");
  });
  it("does not use measurements from a different thinking level", async () => {
    const route = await resolveThreadRoute(jev(), "task", policy({ objective: "time", estimates, frontier_thinking: "low" }));
    expect(route.selection).toBe("prior");
    expect(route.estimate).toBeNull();
  });
  it("does not let estimates override modality or classification fallback", async () => {
    const route = await resolveThreadRoute(jev("terminal", .1), "task", policy({ estimates }));
    expect(route.backend).toBe("chatgpt"); expect(route.selection).toBe("fallback");
  });
  it("validates policy and incompatible creation settings", () => {
    expect(() => policy({ weights: { cost: 0, time: 0, effectiveness: 0 } })).toThrow();
    expect(() => policy({ estimates: [{ ...estimates[0], success_rate: 0 }] })).toThrow();
    expect(() => parseAgentCreateBody(JSON.stringify({ settings: {}, configuration: { model_routing: {} } }))).toThrow();
    expect(() => validateAgentSettings({ model: OSS_MODEL, thinking: "max", fast_mode: false, reasoning_mode: "standard" })).toThrow();
  });
  it("admits routed roots with bounded child agents through public creation", () => {
    const configuration = {
      model_routing: { candidates: [`${OSS_MODEL}:low`, `${OSS_MODEL}:medium`] },
      multi_agent: { enabled: true, max_concurrent_subagents: 1 },
    };
    const admitted = parseAgentCreateBody(JSON.stringify({ configuration }));
    expect(admitted.configuration?.multi_agent).toEqual(configuration.multi_agent);
    expect(admitted.configuration?.model_routing?.candidates).toEqual(configuration.model_routing.candidates);
    expect(() => parseConfiguration({ ...configuration,
      multi_agent: { enabled: true, max_concurrent_subagents: 0 } })).toThrow();
  });
  it("singleflights concurrent admissions and retains route across restart", async () => {
    let retained: Awaited<ReturnType<typeof resolveThreadRoute>> | undefined;
    const store = { read: () => retained, commit: (r: NonNullable<typeof retained>) => { retained = r; } };
    const pin = new ThreadRoutePin(store), ai = jev();
    const make = () => resolveThreadRoute(ai, "first task", policy());
    const [a, b] = await Promise.all([pin.resolve(make), pin.resolve(make)]);
    expect(a).toBe(b); expect(ai.run).toHaveBeenCalledTimes(1);
    const restarted = new ThreadRoutePin(store);
    expect(await restarted.resolve(() => { throw new Error("must not reroute"); })).toBe(a);
  });
  it("does not retain a route if atomic commit failed", async () => {
    const ai = jev(), commit = vi.fn(() => { throw new Error("storage failure"); });
    const pin = new ThreadRoutePin({ read: () => undefined, commit });
    await expect(pin.resolve(() => resolveThreadRoute(ai, "task", policy()))).rejects.toThrow("storage failure");
    expect(commit).toHaveBeenCalledOnce();
  });
  it("migrates an existing Astra settings table without changing its row", () => {
    const db = new DatabaseSync(":memory:");
    db.exec(`CREATE TABLE managed_agent_settings (singleton INTEGER PRIMARY KEY, model TEXT CHECK(model IN ('gpt-6-astra')), thinking TEXT, reasoning_mode TEXT, fast_mode INTEGER);
      INSERT INTO managed_agent_settings VALUES (1,'gpt-6-astra','high','standard',0);`);
    const storage = {
      sql: { exec(sql: string, ...args: unknown[]) {
        if (/^\s*(SELECT|PRAGMA)/.test(sql)) { const rows = db.prepare(sql).all(...args as never[]); return { one: () => rows[0], toArray: () => rows }; }
        db.exec(sql); return { toArray: () => [] };
      } },
      transactionSync(fn: () => void) { db.exec("BEGIN"); try { fn(); db.exec("COMMIT"); } catch (e) { db.exec("ROLLBACK"); throw e; } },
    };
    initializeManagedAgentSettingsSchema(storage as never);
    expect(db.prepare("SELECT model FROM managed_agent_settings").get()?.model).toBe(FRONTIER_MODEL);
    db.prepare("UPDATE managed_agent_settings SET model = ?").run(OSS_MODEL);
    initializeManagedAgentSettingsSchema(storage as never);
    expect(db.prepare("SELECT model FROM managed_agent_settings").get()?.model).toBe(OSS_MODEL);
    db.close();
  });
});


describe("live Unified Billing Jev envelopes", () => {
  it("reads completed wrapped answers and usage", async () => {
    const route = await resolveThreadRoute({run:async()=>({state:"Completed",result:{answers:{family:{choice:"terminal",confidence:.99}},usage:{input_tokens:333,output_tokens:38}},gatewayMetadata:{keySource:"Unified"}})}, "Fix build", policy());
    expect(route.backend).toBe("workers_ai");
    expect(route.confidence).toBe(.99);
    expect(route.router_usage).toEqual({input_tokens:333,output_tokens:38});
  });
  it.each(["Pending", "Failed"])("does not accept %s answers", async state => {
    const route = await resolveThreadRoute({run:async()=>({state,result:{answers:{family:{choice:"terminal",confidence:1}}}})}, "Fix build", policy());
    expect(route.selection).toBe("fallback"); expect(route.backend).toBe("chatgpt");
  });
});

describe("v2 direct candidate routing", () => {
  const direct = (patch = {}) => routingPolicySchema.parse(patch);
  const answer = (choice = "gpt-5.6-luna:low", candidateConfidence = .98, familyConfidence = .97) => ({
    run: vi.fn(async (_model: string, _input: unknown) => ({ answers: {
      candidate: { choice, confidence: candidateConfidence }, family: { choice: "terminal", confidence: familyConfidence },
    } })),
  });
  it("defaults to one direct typed choice across fifteen model/effort candidates", async () => {
    const ai = answer();
    const route = await resolveThreadRoute(ai, "Fix build quickly and cheaply", direct());
    expect(route).toMatchObject({ policy_version: "jev-direct-v3", model: "gpt-5.6-luna", thinking: "low", estimate: null });
    expect(ai.run).toHaveBeenCalledOnce();
    const request = ai.run.mock.calls[0][1] as { state: string; questions: { candidate: { criteria: object } } };
    expect(Object.keys(request.questions.candidate.criteria)).toHaveLength(15);
    expect(JSON.parse(request.state)).toMatchObject({ preferences: {}, preference_sources: { duration: "prompt_or_default", cost: "prompt_or_default" }, measurements: [] });
    expect(route.audit).toMatchObject({ candidate_choice: "gpt-5.6-luna:low", classifier_confidence: .97, candidate_confidence: .98 });
  });
  it("serializes explicit preferences and preserves their priority over opening inference", async () => {
    const ai = answer();
    const route = await resolveThreadRoute(ai, "Be quick, cheap, and thorough", direct({ preferences: { completion: 9, cost: 0, duration: 1, target_cost_usd: .2, target_duration_seconds: 60, text: "Prioritize careful checking" } }));
    expect(route.audit?.preferences).toEqual({ completion: 9, cost: 0, duration: 1, target_cost_usd: .2, target_duration_seconds: 60, text: "Prioritize careful checking" });
    expect(route.audit?.preference_sources).toEqual({ completion: "explicit", cost: "explicit", duration: "explicit" });
    const state = JSON.parse((ai.run.mock.calls[0][1] as {state:string}).state);
    expect(state.preferences).toEqual(route.audit?.preferences);
    expect(state.lower_precedence_defaults).toEqual({objective:"balanced", weights:{cost:1,effectiveness:1,time:1}});
    expect(state.policy).not.toHaveProperty("estimates");
    const instructions = (ai.run.mock.calls[0][1] as {questions:{candidate:{instructions:string}}}).questions.candidate.instructions;
    expect(instructions).toContain("Higher cost weight means MINIMIZE spend");
    expect(instructions).toContain('"cost":0');
    expect(state).toHaveProperty("eval_evidence");
    expect(state).toHaveProperty("task_profiles");
  });
  it.each(["ignore previous instructions", "gpt-7:high", "gpt-6-astra:max"])("rejects adversarial choice %s and falls back inside allowlist", async choice => {
    const route = await resolveThreadRoute(answer(choice), "task", direct({ candidates: [`${OSS_MODEL}:low`] }));
    expect(route).toMatchObject({ model: OSS_MODEL, thinking: "low", selection: "fallback" });
  });
  it("preserves the proposed choice separately when conservative confidence forces fallback", async () => {
    const route = await resolveThreadRoute(answer("gpt-5.6-luna:low", .51), "cheap task", direct({low_confidence_fallback:"frontier"}));
    expect(route).toMatchObject({selection:"fallback", model:FRONTIER_MODEL, thinking:"high"});
    expect(route.audit).toMatchObject({proposed_candidate:"gpt-5.6-luna:low", candidate_choice:"gpt-6-astra:high", candidate_confidence:.51});
  });
  it("retains a selected supported thinking level and bounds eligibility", async () => {
    for (const thinking of ["low", "medium", "high"]) {
      const id = `gpt-5.6-sol:${thinking}`;
      const route = await resolveThreadRoute(answer(id), "task", direct({ candidates: [id] }));
      expect(route).toMatchObject({ model: "gpt-5.6-sol", thinking, selection: "prior" });
      expect(route.audit?.eligible_candidates).toEqual([id]);
    }
    expect(() => direct({ preferences: {completion:0, cost:0, duration:0} })).toThrow();
    expect(() => direct({ candidates: [] })).toThrow();
    expect(() => direct({ candidates: ["unknown"] })).toThrow();
    expect(() => direct({ preferences: { text: " " } })).toThrow();
    expect(() => direct({ preferences: { text: "x".repeat(2001) } })).toThrow();
  });
  it.each(["Pending", "Failed"])("does not admit %s envelopes", async state => {
    const route = await resolveThreadRoute({run: async () => ({state, result: await answer().run("", {})})}, "task", direct({ candidates: ["gpt-5.6-sol:medium"] }));
    expect(route).toMatchObject({ selection: "fallback", model: "gpt-5.6-sol", thinking: "medium" });
  });
  it("accepts completed envelopes and retains usage", async () => {
    const route = await resolveThreadRoute({run: async () => ({state: "Completed", result: {...await answer().run("", {}), usage: { input_tokens: 42 }}})}, "task", direct());
    expect(route.router_usage).toEqual({input_tokens:42});
    expect(route.model).toBe("gpt-5.6-luna");
  });
  it("rejects a modality with no eligible model and bounds oversized fallback", async () => {
    const ai = answer();
    await expect(resolveThreadRoute(ai, [{type:"input_image"}], direct({candidates:[`${OSS_MODEL}:low`]}))).rejects.toThrow("no route admitted");
    expect((await resolveThreadRoute(ai, "x".repeat(24001), direct({candidates:["gpt-5.6-sol:low"]}))).model).toBe("gpt-5.6-sol");
    expect(ai.run).not.toHaveBeenCalled();
  });
  it("never substitutes classifier confidence or wrong effort evidence for measured success", async () => {
    const measurement = {family:"terminal", backend:"chatgpt", model:"gpt-5.6-luna", thinking:"low", success_rate:.8, expected_cost_usd:.1, expected_duration_ms:1000, sample_size:20, source:"heldout-v2"};
    const p = direct({min_success_rate:.75, estimates:[measurement]});
    expect((await resolveThreadRoute(answer("gpt-5.6-luna:low", .98, .1), "not cheap; take your time", p)).selection).toBe("prior");
    expect((await resolveThreadRoute(answer(), "task", p)).estimate?.success_rate).toBe(.8);
    for (const ai of [answer("gpt-5.6-luna:high"), answer("gpt-5.6-luna:low", .2), answer("unknown")]) {
      await expect(resolveThreadRoute(ai, "task", p)).rejects.toThrow("no route admitted");
    }
    await expect(resolveThreadRoute(answer(), "task", direct({min_success_rate:.9, estimates:[measurement]}))).rejects.toThrow("no route admitted");
    await expect(resolveThreadRoute(answer(), "task", direct({min_success_rate:.5}))).rejects.toThrow("no route admitted");
  });
  it("labels measured comparisons only for a complete matched source cohort", async () => {
    const base = {family:"terminal", backend:"chatgpt", model:"gpt-5.6-luna", thinking:"low", success_rate:.8, expected_cost_usd:.1, expected_duration_ms:1000, sample_size:20, source:"heldout-v2"};
    const candidates = ["gpt-5.6-luna:low", "gpt-5.6-sol:low"];
    const other = {...base, model:"gpt-5.6-sol"};
    expect((await resolveThreadRoute(answer(), "task", direct({candidates, estimates:[base, other]}))).selection).toBe("measured");
    expect((await resolveThreadRoute(answer(), "task", direct({candidates, estimates:[base, {...other, source:"different"}]}))).selection).toBe("prior");
  });
  it("pins the direct model/effort and audit across concurrent admissions and restart", async () => {
    let retained: Awaited<ReturnType<typeof resolveThreadRoute>> | undefined;
    const store = { read: () => retained, commit: (r: NonNullable<typeof retained>) => {retained = r;} };
    const pin = new ThreadRoutePin(store), ai = answer();
    const create = () => resolveThreadRoute(ai, "quick task", direct());
    const [a,b] = await Promise.all([pin.resolve(create), pin.resolve(create)]);
    expect(a).toBe(b);
    expect(await new ThreadRoutePin(store).resolve(create)).toBe(a);
    expect(ai.run).toHaveBeenCalledOnce();
  });
});


describe("cross-provider candidate routing", () => {
  const available = { openrouter: true, vercel: true };
  const choose = (id: string) => ({ run: vi.fn(async (_model: string, _input: unknown) => ({ answers: {
    candidate: { choice: id, confidence: .99 }, family: { choice: "terminal", confidence: .99 },
  } })) });
  const openrouter = "openrouter:openai/gpt-6-astra:high";
  const vercel = "vercel:openai/gpt-6-astra:high";
  it("offers 45 unique candidates, preserving every old identity", async () => {
    expect(ROUTING_CANDIDATES).toHaveLength(45);
    expect(new Set(ROUTING_CANDIDATES.map(c => c.id)).size).toBe(45);
    const p = routingPolicySchema.parse({ candidates: ROUTING_CANDIDATES.map(c => c.id) });
    const ai = choose(vercel);
    const route = await resolveThreadRoute(ai, "task", p, available);
    expect(route.audit?.eligible_candidates).toHaveLength(45);
    expect(route).toMatchObject({ backend: "vercel", model: FRONTIER_MODEL, provider_model: "openai/gpt-6-astra" });
    expect(JSON.parse((ai.run.mock.calls[0][1] as {state:string}).state).candidates).toHaveLength(45);
  });
  it("sends dated provider token rates separately from measured task costs", async () => {
    const ai = choose(vercel);
    const route = await resolveThreadRoute(ai, "compare cost", routingPolicySchema.parse({}), available);
    const state = JSON.parse((ai.run.mock.calls[0][1] as {state:string}).state);
    const hint = (id: string) => state.candidates.find((c: {id:string}) => c.id === id).catalog_price_hint;
    expect(hint("openrouter:openai/gpt-5.6-sol:low")).toMatchObject({as_of:"2026-09-20", unit:"USD per million tokens", input:2, output:10, cached_input:.2, source:"https://openrouter.ai/api/v1/models"});
    expect(hint("vercel:openai/gpt-5.6-sol:low")).toMatchObject({input:4, output:20, cached_input:.4, source:"https://ai-gateway.vercel.sh/v1/models"});
    expect(hint("openrouter:z-ai/glm-5.3:low")).toMatchObject({input:.91,output:2.86,cached_input:.169});
    expect(hint("vercel:zai/glm-5.3:low")).toMatchObject({input:1.4,output:4.4,cached_input:.14});
    expect(hint("gpt-6-astra:high")).toBeNull();
    expect(hint(vercel).note).toContain("exclude long-context tiers");
    expect(hint(vercel)).not.toHaveProperty("expected_duration_ms");
    expect(hint(vercel)).not.toHaveProperty("expected_cost_usd");
    expect(state.measurements).toEqual([]);
    expect(route.estimate).toBeNull();
    expect(route.selection).toBe("prior");
  });
  it.each([
    [undefined, 15], [{openrouter:true,vercel:false},30], [{openrouter:false,vercel:true},30], [available,45],
  ])("filters unavailable providers before Jev: %j", async (availability, count) => {
    const ai = choose("gpt-6-astra:high");
    const route = await resolveThreadRoute(ai, "task", routingPolicySchema.parse({}), availability);
    expect(route.audit?.eligible_candidates).toHaveLength(count);
    expect(Object.keys((ai.run.mock.calls[0][1] as {questions:{candidate:{criteria:object}}}).questions.candidate.criteria)).toHaveLength(count);
  });
  it("rejects an unavailable-only allowlist before Jev and never expands fallback", async () => {
    const ai = choose(openrouter);
    await expect(resolveThreadRoute(ai, "task", routingPolicySchema.parse({candidates:[openrouter]}))).rejects.toThrow("no route admitted");
    expect(ai.run).not.toHaveBeenCalled();
    const route = await resolveThreadRoute(choose("unknown"), "task", routingPolicySchema.parse({candidates:[openrouter,vercel]}), {openrouter:false,vercel:true});
    expect(route.audit?.candidate_choice).toBe(vercel);
    expect(route.audit?.eligible_candidates).toEqual([vercel]);
  });
  it.each([["openrouter", "z-ai/glm-5.3"], ["vercel", "zai/glm-5.3"]])("pins %s provider endpoint alongside canonical identity across restart", async (backend, provider_model) => {
    const id = `${backend}:${provider_model}:medium`;
    let retained: Awaited<ReturnType<typeof resolveThreadRoute>> | undefined;
    const store = {read:()=>retained, commit:(r: NonNullable<typeof retained>)=>{retained = JSON.parse(JSON.stringify(r));}};
    await new ThreadRoutePin(store).resolve(()=>resolveThreadRoute(choose(id), "task", routingPolicySchema.parse({candidates:[id]}), available));
    const restored = await new ThreadRoutePin(store).resolve(()=>{throw new Error("unexpected reroute");});
    expect(restored).toMatchObject({ backend, provider_model, model: OSS_MODEL, thinking:"medium" });
  });
  it("uses provider-specific measurements for identical canonical model and effort", async () => {
    const base = {family:"terminal", model:FRONTIER_MODEL, thinking:"high", success_rate:.8, expected_cost_usd:.1, expected_duration_ms:1000, sample_size:20, source:"heldout-provider-v1"};
    const estimates = [{...base,backend:"openrouter"}, {...base,backend:"vercel",expected_cost_usd:.5,success_rate:.95}];
    const p = routingPolicySchema.parse({candidates:[openrouter,vercel],estimates,min_success_rate:.9});
    const route = await resolveThreadRoute(choose(vercel), "task", p, available);
    expect(route.selection).toBe("measured");
    expect(route.estimate).toMatchObject({backend:"vercel",expected_cost_usd:.5});
    await expect(resolveThreadRoute(choose(openrouter), "task", p, available)).rejects.toThrow("no route admitted");
    expect(()=>routingPolicySchema.parse({estimates:[{...base,backend:"workers_ai"}]})).toThrow();
    expect(()=>routingPolicySchema.parse({estimates:[{...base,backend:"chatgpt",model:OSS_MODEL}]})).toThrow();
  });
  it("keeps legacy comparison restricted to its original two providers", async () => {
    const base = {family:"terminal", thinking:"high", success_rate:.9, expected_cost_usd:.3, expected_duration_ms:6000, sample_size:100, source:"heldout-v1"};
    const estimates = [{...base,backend:"chatgpt",model:FRONTIER_MODEL}, {...base,backend:"workers_ai",model:OSS_MODEL,thinking:"medium"}, {...base,backend:"vercel",model:FRONTIER_MODEL,expected_cost_usd:0}];
    const route = await resolveThreadRoute(jev(), "task", policy({estimates,objective:"cost"}), available);
    expect(["workers_ai","chatgpt"]).toContain(route.backend);
    expect(route.provider_model).toBe(route.model);
  });
});


describe("trusted regional provider telemetry", () => {
  const id = "openrouter:openai/gpt-6-astra:high";
  const metric = (patch = {}) => ({backend:"openrouter",model:"openai/gpt-6-astra",effort:"high",source:"live",workerColo:"LHR",
    signalKind:"context_only_not_completion_probability",usable:true,sampleCount:6,successCount:5,censoredCount:1,
    successRate:5/6,lastObservedAt:Date.now()-1000,fullResponseP50Ms:120,fullResponseEwmaMs:140,...patch});
  const ai = () => ({run:vi.fn(async (_model:string,_input:unknown)=>({answers:{candidate:{choice:id,confidence:.99},family:{choice:"terminal",confidence:.99}}}))});
  const runtime = (provider_performance: unknown[]) => ({openrouter:true,vercel:false,workerColo:"LHR",clientIngressColo:"SJC",provider_performance});
  it("projects trusted aggregates into Jev and the audit while distinguishing execution from ingress", async () => {
    const router = ai();
    const route = await resolveThreadRoute(router,"task",routingPolicySchema.parse({}),runtime([metric({apiKey:"secret",prompt:"private",errorBody:"sensitive"}),metric({source:"probe"})]));
    const snapshot = route.audit?.provider_telemetry;
    expect(snapshot).toMatchObject({provenance:"trusted_runtime_aggregate",workerColo:"LHR",clientIngressColo:"SJC",windowMs:7200000});
    expect(snapshot?.provider_performance).toHaveLength(2);
    expect(snapshot?.provider_performance[0]).toMatchObject({model:FRONTIER_MODEL,fullResponseP50Ms:120});
    const state = JSON.parse((router.run.mock.calls[0][1] as {state:string}).state);
    expect(state.provider_telemetry).not.toHaveProperty("provider_performance");
    expect(state.provider_telemetry).toMatchObject({ provenance: "trusted_runtime_aggregate", workerColo: "LHR" });
    expect(state.candidates.find((c: {id:string}) => c.id === id).availability.live).toMatchObject({ sampleCount: 6, failureCount: 1 });
    for (const privateField of ["apiKey","prompt","errorBody","successRate"]) expect(snapshot?.provider_performance[0]).not.toHaveProperty(privateField);
    expect(route.estimate).toBeNull();
    expect(route.selection).toBe("prior");
  });
  it("rejects stale, future, inconsistent, wrong-region, unknown and unavailable groups", async () => {
    const samples = [metric({lastObservedAt:Date.now()-300001}),metric({lastObservedAt:Date.now()+60000}),metric({successCount:4}),metric({workerColo:"SJC"}),metric({model:"unlisted"}),metric({backend:"vercel"}),metric({effort:null}),metric({fullResponseP50Ms:Infinity}),metric({successCount:20})];
    const route = await resolveThreadRoute(ai(),"task",routingPolicySchema.parse({}),runtime(samples));
    expect(route.audit?.provider_telemetry?.provider_performance).toEqual([]);
    const unknownColo = await resolveThreadRoute(ai(),"task",routingPolicySchema.parse({}),{...runtime([metric()]),workerColo:null});
    expect(unknownColo.audit?.provider_telemetry?.provider_performance).toEqual([]);
  });
  it("retains availability-only context without presenting it as generation latency", async () => {
    const router = ai();
    const route = await resolveThreadRoute(router,"task",routingPolicySchema.parse({}),runtime([metric({usable:false})]));
    expect(route.audit?.provider_telemetry?.provider_performance).toHaveLength(1);
    expect(route.audit?.provider_telemetry?.provider_performance[0]).toMatchObject({
      sampleCount:6,successCount:5,availabilityFailureCount:1,usable:false,ttftUsable:false,
      generationTtftP50Ms:null,generationTtftEwmaMs:null,generationTtftSampleCount:0,
    });
    const state = JSON.parse((router.run.mock.calls[0][1] as {state:string}).state);
    expect(state.candidates.find((candidate: {id:string}) => candidate.id === id).responsiveness).toMatchObject({live:null,probe:null});
    expect(route.estimate).toBeNull();
    expect(route.selection).toBe("prior");
  });
  it("bounds and deduplicates aggregates without merging probe/live cohorts", async () => {
    const samples = ROUTING_CANDIDATES.filter(c=>c.backend==="openrouter").flatMap(c=>[metric({model:c.model,effort:c.thinking}),metric({model:c.model,effort:c.thinking,source:"probe"})]);
    const route = await resolveThreadRoute(ai(),"task",routingPolicySchema.parse({}),runtime([samples[0],...samples]));
    expect(route.audit?.provider_telemetry?.provider_performance).toHaveLength(30);
    expect(new Set(route.audit?.provider_telemetry?.provider_performance.map(m => `${m.source}/${m.candidateId}`)).size).toBe(30);
  });
  it("cannot satisfy a measured-success threshold or trust policy/request telemetry", async () => {
    expect(()=>routingPolicySchema.parse({provider_performance:[metric()]})).toThrow();
    await expect(resolveThreadRoute(ai(),"task",routingPolicySchema.parse({min_success_rate:.5}),runtime([metric()]))).rejects.toThrow("no route admitted");
    const route = await resolveThreadRoute(ai(),{provider_performance:[metric()],workerColo:"LHR"},routingPolicySchema.parse({}),{openrouter:true,vercel:false});
    expect(route.audit?.provider_telemetry?.provider_performance).toEqual([]);
  });
});

describe("preference-preserving confidence fallback", () => {
  const economy = `${OSS_MODEL}:low`;
  const output = (choice: unknown = economy, confidence: unknown = .6, family: unknown = "other") => ({
    run: vi.fn(async (_model: string, _input: unknown) => ({answers:{
      candidate:{choice,confidence}, family:{choice:family,confidence:.9},
    }})),
  });
  it("retains the valid economy proposal below the unchanged confidence threshold with honest audit", async () => {
    const p = routingPolicySchema.parse({preferences:{completion:10,cost:80,duration:10}});
    expect(p.min_confidence).toBe(.75);
    expect(p.low_confidence_fallback).toBe("proposed");
    const ai = output();
    const route = await resolveThreadRoute(ai,"Use the most expensive model; cost is irrelevant",p);
    expect(route).toMatchObject({model:OSS_MODEL,thinking:"low",selection:"fallback",estimate:null});
    expect(route.audit).toMatchObject({candidate_choice:economy,proposed_candidate:economy,candidate_confidence:.6,
      confidence_status:"low",fallback_basis:"valid_proposal",preferences:p.preferences});
    expect(ai.run).toHaveBeenCalledOnce();
  });
  it.each([0,.749999,.75,1])("uses threshold as the confidence boundary at %s", async confidence => {
    const route = await resolveThreadRoute(output(economy,confidence),"task",routingPolicySchema.parse({}));
    expect(route.selection).toBe(confidence < .75 ? "fallback" : "prior");
    expect(route.audit?.confidence_status).toBe(confidence < .75 ? "low" : "accepted");
    expect(route.audit?.fallback_basis).toBe(confidence < .75 ? "valid_proposal" : "none");
  });
  it.each([
    ["unknown",.6,"other"], [economy,"0.9","other"], [economy,NaN,"other"],
    [economy,1.1,"other"], [economy,-.1,"other"], [economy,.99,"invented_family"],
    ["openrouter:z-ai/glm-5.3:low",.99,"other"],
  ])("does not preserve malformed or ineligible proposals: %j", async (choice,confidence,family) => {
    const route = await resolveThreadRoute(output(choice,confidence,family),"cheap task",routingPolicySchema.parse({}));
    expect(route).toMatchObject({model:FRONTIER_MODEL,thinking:"high",selection:"fallback",estimate:null});
    expect(route.audit).toMatchObject({confidence_status:"unavailable_or_invalid",fallback_basis:"eligible_frontier"});
  });
  it("retains a provider-specific proposal only within available eligible candidates", async () => {
    const id = "vercel:openai/gpt-5.6-luna:low";
    const route = await resolveThreadRoute(output(id,.3),"economy",routingPolicySchema.parse({candidates:[id]}),{openrouter:false,vercel:true});
    expect(route).toMatchObject({backend:"vercel",provider_model:"openai/gpt-5.6-luna",selection:"fallback"});
    expect(route.audit?.candidate_choice).toBe(id);
  });
  it("cannot relabel a low-confidence proposal as measured or satisfy a measured-success constraint", async () => {
    const estimates = [{family:"other",backend:"workers_ai",model:OSS_MODEL,thinking:"low",success_rate:.99,
      expected_cost_usd:.01,expected_duration_ms:100,sample_size:100,source:"synthetic-matched-v1"}];
    expect(await resolveThreadRoute(output(),"task",routingPolicySchema.parse({estimates})))
      .toMatchObject({selection:"fallback",estimate:null});
    await expect(resolveThreadRoute(output(),"task",routingPolicySchema.parse({estimates,min_success_rate:.9})))
      .rejects.toThrow("no route admitted");
  });
  it("pins a low-confidence proposal once across concurrent admission and restart", async () => {
    let retained: Awaited<ReturnType<typeof resolveThreadRoute>> | undefined;
    const store = {read:()=>retained,commit:(route:NonNullable<typeof retained>)=>{retained=route;}};
    const ai = output(), pin = new ThreadRoutePin(store);
    const create = ()=>resolveThreadRoute(ai,"economy",routingPolicySchema.parse({}));
    const [a,b] = await Promise.all([pin.resolve(create),pin.resolve(create)]);
    expect(a).toBe(b);
    expect(await new ThreadRoutePin(store).resolve(create)).toBe(a);
    expect(a.audit?.fallback_basis).toBe("valid_proposal");
    expect(ai.run).toHaveBeenCalledOnce();
  });
});


describe("captured preference-distribution policy replay (not accuracy labels)", () => {
  it.each(preferenceObservations.cases)("retains $catalog/$id without inflating confidence", async observation => {
    const ai = {run:vi.fn(async()=>({state:"Completed",result:{answers:observation.answers}}))};
    const route = await resolveThreadRoute(ai,observation.prompt,routingPolicySchema.parse({preferences:observation.preferences}),
      {openrouter:observation.catalog===45,vercel:observation.catalog===45});
    expect(route.audit?.candidate_choice).toBe(observation.expected_choice);
    expect(route.audit?.candidate_confidence).toBe(observation.answers.candidate.confidence);
    expect(route.selection).toBe(observation.expected_selection);
    expect(route.estimate).toBeNull();
    expect(ai.run).toHaveBeenCalledOnce();
    const state = JSON.parse((ai.run.mock.calls[0] as unknown as [string,{state:string}])[1].state);
    expect(state.preferences).toEqual(observation.preferences);
    expect(state.candidates).toHaveLength(observation.catalog);
  });
});


it("reuses an old v2 pinned route without applying v3 defaults or rerouting", async () => {
  const old = await resolveThreadRoute({run:async()=>({answers:{candidate:{choice:"gpt-6-astra:high",confidence:.9},family:{choice:"other",confidence:.9}}})},
    "existing thread",routingPolicySchema.parse({}));
  old.policy_version = "jev-direct-v2";
  delete old.audit!.confidence_status;
  delete old.audit!.fallback_basis;
  const create = vi.fn(async()=>{throw Error("old thread must not reroute");});
  const commit = vi.fn();
  expect(await new ThreadRoutePin({read:()=>old,commit}).resolve(create)).toBe(old);
  expect(create).not.toHaveBeenCalled();
  expect(commit).not.toHaveBeenCalled();
  expect(old.policy_version).toBe("jev-direct-v2");
});


describe("public Jev route diagnostics", () => {
  const economy = `${OSS_MODEL}:low`, frontier = `${FRONTIER_MODEL}:high`;
  const candidates = [economy, frontier];
  const candidateProbabilities = { [economy]: .87, [frontier]: .13 };
  const familyProbabilities = Object.fromEntries(taskFamily.options.map(f => [f, f === "terminal" ? 1 : 0]));
  const payload = () => ({ answers: {
    candidate: { choice: economy, confidence: .8, probabilities: candidateProbabilities },
    family: { choice: "terminal", confidence: .94, probabilities: familyProbabilities },
  }, usage: { echoed: "private input" }, audit: "private input" });
  const routeFor = (result: unknown, patch = {}) => resolveThreadRoute({ run: async () => result },
    "private input", routingPolicySchema.parse({ candidates, preferences: { text: "private preference" }, ...patch }));

  it.each([false, true])("preserves actual probabilities separately from confidence (wrapped=%s)", async wrapped => {
    const result = payload();
    const route = await routeFor(wrapped ? { state: "Completed", result } : result);
    expect(projectThreadRouteDiagnostics(route)).toEqual({
      source: "typesafe/jev", signal_kind: "choice_probabilities_and_confidence_not_task_success",
      eligible_candidates: candidates, chosen_candidate: economy, proposed_candidate: economy,
      candidate_confidence: .8, family_confidence: .94,
      candidate_probabilities: candidateProbabilities, family_probabilities: familyProbabilities,
      min_confidence: .75, confidence_status: "accepted", fallback_basis: "none",
    });
    expect(JSON.stringify(projectThreadRouteDiagnostics(route))).not.toContain("private");
  });

  it("preserves two-decimal rounded probabilities without renormalizing", async () => {
    const result = payload();
    result.answers.candidate.probabilities = { [economy]: .86, [frontier]: .13 };
    const route = await routeFor(result);
    expect(projectThreadRouteDiagnostics(route)?.candidate_probabilities).toEqual({ [economy]: .86, [frontier]: .13 });
  });

  it.each([undefined, null, {}, [1, 0], { [economy]: 1 },
    { [economy]: .8, "private input": .2 }, { ...candidateProbabilities, "private input": 0 },
    { [economy]: "0.87", [frontier]: .13 }, { [economy]: NaN, [frontier]: .13 },
    { [economy]: Infinity, [frontier]: 0 }, { [economy]: -.1, [frontier]: 1.1 },
    { [economy]: .2, [frontier]: .2 },
  ])("omits absent or malformed distributions without inventing probabilities %#", async probabilities => {
    const result = payload();
    const route = await routeFor({ ...result, answers: { ...result.answers,
      candidate: { ...result.answers.candidate, probabilities } } });
    expect(route.selection).toBe("prior");
    expect(projectThreadRouteDiagnostics(route)).toMatchObject({ candidate_confidence: .8,
      candidate_probabilities: null, family_probabilities: familyProbabilities });
  });

  it.each(["proposed", "frontier"])("reports low confidence and %s fallback without changing probabilities", async low_confidence_fallback => {
    const result = payload(); result.answers.candidate.confidence = .2;
    const route = await routeFor(result, { low_confidence_fallback });
    expect(projectThreadRouteDiagnostics(route)).toMatchObject({ proposed_candidate: economy,
      chosen_candidate: low_confidence_fallback === "proposed" ? economy : frontier,
      candidate_confidence: .2, family_confidence: .94, min_confidence: .75,
      confidence_status: "low", fallback_basis: low_confidence_fallback === "proposed" ? "valid_proposal" : "eligible_frontier",
      candidate_probabilities: candidateProbabilities, family_probabilities: familyProbabilities });
  });

  it.each(["candidate", "family"])("never projects echoed invalid %s choices", async field => {
    const result = payload(); result.answers[field as "candidate" | "family"].choice = "private input";
    const route = await routeFor(result);
    expect(projectThreadRouteDiagnostics(route)).toMatchObject({ proposed_candidate: null, chosen_candidate: frontier,
      candidate_confidence: null, family_confidence: null, candidate_probabilities: null, family_probabilities: null,
      confidence_status: "unavailable_or_invalid", fallback_basis: "eligible_frontier" });
    expect(JSON.stringify(projectThreadRouteDiagnostics(route))).not.toContain("private");
  });

  it("checks family keys, eligible candidate keys and bounds again at the public projection", async () => {
    const route = await routeFor(payload());
    route.audit!.family_probabilities = { "private input": 1 };
    expect(projectThreadRouteDiagnostics(route)?.family_probabilities).toBeNull();
    route.audit!.candidate_probabilities = { [economy]: 2, [frontier]: -1 };
    expect(projectThreadRouteDiagnostics(route)?.candidate_probabilities).toBeNull();
    route.audit!.eligible_candidates.push("private input");
    expect(projectThreadRouteDiagnostics(route)).toBeUndefined();
  });

  it("omits the optional projection for older pins without diagnostics", async () => {
    const route = await routeFor(payload());
    delete route.audit!.confidence_status;
    expect(projectThreadRouteDiagnostics(route)).toBeUndefined();
    delete route.audit;
    expect(projectThreadRouteDiagnostics(route)).toBeUndefined();
  });
});
