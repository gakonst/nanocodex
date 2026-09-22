import { z } from "zod";
import { resolveThreadRoute, ROUTING_CANDIDATES, routingPolicySchema, type RoutingAi,
  type ThreadRoute, type ThreadRoutingPolicy, type RoutingAvailability } from "./thread-model-routing";
const requestSchema = z.object({parentSessionId:z.string().min(1).max(256),role:z.string().max(4096),task:z.string().max(65536),
  model:z.string().optional(),thinking:z.enum(["low","medium","high"]).optional(),hostContextRef:z.string().min(1).max(256)}).strict();
const aliases:Record<string,string>={sol:"gpt-5.6-sol",terra:"gpt-5.6-terra",luna:"gpt-5.6-luna",astra:"gpt-6-astra","glm-5.3":"@cf/zai-org/glm-5.3"};
export type RetainedChildRoute={routeId:string;parentSessionId:string;hostContextRef:string;route:ThreadRoute};
export interface ChildRouteStore { read(sessionId:string):RetainedChildRoute|undefined; commit(sessionId:string,value:RetainedChildRoute):void; }
/** One decision per newly spawned child, committed before its first inference. No secrets in records. */
export function createSubagentRouteController(options:{ai:RoutingAi;policy:ThreadRoutingPolicy;availability:()=>RoutingAvailability;
  store:ChildRouteStore;authorize:(parentSessionId:string,hostContextRef:string)=>void;id?:()=>string}) {
  const pending=new Map<string,RetainedChildRoute>();
  return {
    async resolve(raw:unknown){
      const r=requestSchema.parse(raw);options.authorize(r.parentSessionId,r.hostContextRef);
      if(pending.size>=64)throw Error("Too many pending child routes");
      const model=r.model===undefined?undefined:aliases[r.model]??r.model;
      const candidates=ROUTING_CANDIDATES.filter(c=>(!options.policy.candidates||options.policy.candidates.includes(c.id))
        &&(model===undefined||c.model===model)&&(r.thinking===undefined||c.thinking===r.thinking)).map(c=>c.id);
      if(!candidates.length)throw Error("Explicit child model/effort is outside eligible routing policy");
      const route=await resolveThreadRoute(options.ai,JSON.stringify({role:r.role,task:r.task}),
        routingPolicySchema.parse({...options.policy,strategy:"direct",candidates}),options.availability());
      options.authorize(r.parentSessionId,r.hostContextRef);
      if(pending.size>=64)throw Error("Too many pending child routes");
      const routeId=(options.id??crypto.randomUUID)();
      pending.set(routeId,{routeId,parentSessionId:r.parentSessionId,hostContextRef:r.hostContextRef,route});
      return {model:route.model,thinking:route.thinking,routeId};
    },
    bind(raw:unknown){
      const r=z.object({parentSessionId:z.string(),sessionId:z.string().min(1).max(256),routeId:z.string(),hostContextRef:z.string()}).strict().parse(raw);
      options.authorize(r.parentSessionId,r.hostContextRef);
      const retained=options.store.read(r.sessionId);
      if(retained){if(retained.routeId!==r.routeId||retained.parentSessionId!==r.parentSessionId||retained.hostContextRef!==r.hostContextRef)throw Error("Child route conflicts with retained binding");return;}
      const proposed=pending.get(r.routeId);
      if(!proposed||proposed.parentSessionId!==r.parentSessionId||proposed.hostContextRef!==r.hostContextRef)throw Error("Unknown or unauthorized child route reference");
      options.store.commit(r.sessionId,proposed);pending.delete(r.routeId);
    },
    routeForSession(sessionId:string){return options.store.read(sessionId)?.route;},
  };
}
