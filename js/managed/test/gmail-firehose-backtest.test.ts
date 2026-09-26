import { describe, expect, it } from "vitest";
import { jevGatewayBinding, routeGmailDecisionBacktest } from "../src/gmail-firehose-backtest";
import type { RoutingAi } from "../src/thread-model-routing";
import { validGmailDecisionTrace } from "../src/gmail-firehose-traces";

const sample = (id: string, expected: "reply" | "no_reply", body: string) => ({id, expected,
  from:"Person <person@example.test>", subject:"Question", body});
const request = (samples: unknown[], headers: HeadersInit = {}) => new Request("https://example.test/v1/todo/decision-backtest", {
  method:"POST",headers:{"content-type":"application/json",origin:"https://example.test",...headers},
  body:JSON.stringify({samples}),
});
const model: RoutingAi = {run:async (_model,input) => {
  const state = JSON.parse((input as any).state);
  return {state:"Completed",result:{answers:{action:{choice:state.body.includes("reply please")?"reply_requested":"no_reply",
    confidence:state.body.includes("reply please")?0.9:0.97, probabilities:{reply_requested:0.9,no_reply:0.1}}}}};
}};
const owner = {kind:"api_key",userId:"owner",capabilities:["agents:write"],connectGrant:undefined};

describe("private Gmail decision backtests", () => {
  it("reports a per-threshold confusion matrix without echoing the fixture or persisting it", async () => {
    const response = await routeGmailDecisionBacktest(request([
      sample("a","reply","reply please -- private test string"),sample("b","no_reply","newsletter")]),
      model, owner as any, "owner");
    expect(response?.status).toBe(200);
    const result = await response!.json() as any;
    expect(result.rows).toHaveLength(2);
    expect(result.thresholds["0.85"]).toMatchObject({true_positive:1,true_negative:1,false_positive:0,false_negative:0});
    expect(JSON.stringify(result)).not.toContain("private test string");
    expect(JSON.stringify(result)).not.toContain("person@example.test");
  });
  it("routes Jev via the selected gateway without retaining private prompt logs", async () => {
    let options: unknown;
    const ai = jevGatewayBinding({run:async (_model:string,_input:unknown, settings:unknown) => {
      options = settings;return {};
    }} as any,"my-gateway");
    await ai.run("typesafe/jev",{state:"fixture"});
    expect(options).toEqual({gateway:{id:"my-gateway",collectLog:false,skipCache:true}});
    expect(() => jevGatewayBinding(model,"bad gateway!")).toThrow("invalid_jev_gateway_id");
  });
  it("refuses audit records carrying arbitrary email content", () => {
    const trace = {source_key:`gmail:gmail-reply-triage-v1:${"a".repeat(64)}`,
      policy_version:"gmail-reply-triage-v1",outcome:"no_reply",reason:"no_reply",
      classifier_outcome:"success",confidence:0.96,reply_probability:0.1,duration_ms:10,decision_id:null} as const;
    expect(validGmailDecisionTrace(trace)).toBe(true);
    expect(validGmailDecisionTrace({...trace,body:"private email"} as any)).toBe(false);
    expect(validGmailDecisionTrace({...trace,confidence:NaN})).toBe(false);
  });
  it("requires account owner write authority, origin and bounded fixture inputs", async () => {
    expect((await routeGmailDecisionBacktest(request([sample("a","reply","reply please")]),model,null,"owner"))?.status).toBe(401);
    expect((await routeGmailDecisionBacktest(request([sample("a","reply","reply please")]),model,{...owner,userId:"other"} as any,"owner"))?.status).toBe(403);
    expect((await routeGmailDecisionBacktest(request([sample("a","reply","reply please")]),model,{...owner,connectGrant:{}} as any,"owner"))?.status).toBe(403);
    expect((await routeGmailDecisionBacktest(request([sample("a","reply","reply please")],{origin:"https://bad.test"}),model,{...owner,kind:"account_session"} as any,"owner"))?.status).toBe(403);
    expect((await routeGmailDecisionBacktest(request([sample("a","reply","x".repeat(9000))]),model,owner as any,"owner"))?.status).toBe(400);
  });
});
