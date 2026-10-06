import type { ToolContext } from "nanocodex";
import type { NativeBashPin, NamespaceExecutionRuntime } from "./namespace-tools";
import type { Tool } from "../../nanocodex/runtime/claude.mjs";

/** Selection and jobs retain immutable native provider identities across DO reopen. */
export function claudeBashJobs(storage: DurableObjectStorage, namespace: Pick<NamespaceExecutionRuntime, "nativeBash">, authorize: (context: ToolContext) => void) {
  storage.sql.exec("CREATE TABLE IF NOT EXISTS managed_claude_bash_selection (session_id TEXT PRIMARY KEY, pin TEXT NOT NULL)");
  storage.sql.exec("CREATE TABLE IF NOT EXISTS managed_claude_bash_jobs (session_id TEXT NOT NULL, task_id TEXT NOT NULL, pin TEXT NOT NULL, PRIMARY KEY(session_id,task_id))");
  const selected = (context: ToolContext) => {
    const row = storage.sql.exec<{pin: string}>("SELECT pin FROM managed_claude_bash_selection WHERE session_id=?", context.sessionId).toArray()[0];
    return row ? JSON.parse(row.pin) as NativeBashPin : undefined;
  };
  const reset = (context: ToolContext) => {authorize(context); storage.sql.exec("DELETE FROM managed_claude_bash_selection WHERE session_id=?", context.sessionId);};
  const invoke = async (name: string, input: Record<string, unknown>, context: ToolContext, pin?: NativeBashPin, workdir?: string) => {
    authorize(context); context.signal.throwIfAborted();
    const result = await namespace.nativeBash(name, input, context, pin, workdir, admitted => {
      authorize(context); context.signal.throwIfAborted();
      if (name === "Bash") storage.sql.exec("INSERT OR REPLACE INTO managed_claude_bash_selection VALUES (?,?)", context.sessionId, JSON.stringify(admitted));
    });
    authorize(context); context.signal.throwIfAborted();
    const output = result.output;
    if (name === "Bash" && !output.is_error) {
      for (const block of output.content as {type: string; text?: string}[]) {
        if (block.type !== "text" || !block.text) continue;
        let body: {task_id?: unknown}; try {body=JSON.parse(block.text);} catch {continue;}
        if (typeof body.task_id === "string" && body.task_id.startsWith("bash-")) storage.sql.exec("INSERT OR REPLACE INTO managed_claude_bash_jobs VALUES (?,?,?)", context.sessionId, body.task_id, JSON.stringify(result.pin));
      }
    }
    return {content: output.content, isError: output.is_error, structuredResult: output.structured_result, metadata: output.metadata};
  };
  const tools: Tool[] = [
    {name:"TaskOutput", description:"Read a native Bash job from its original Hand and session. Jobs do not survive native Hand restart.", inputSchema:{type:"object",properties:{task_id:{type:"string"},block:{type:"boolean",default:true},timeout:{type:"integer",minimum:0,maximum:600000,default:30000}},required:["task_id"],additionalProperties:false},handler:(input,context)=>job("TaskOutput",input,context)},
    {name:"TaskStop", description:"Stop a native Bash job on its original Hand, including retained descendants. Never retries or retargets a missing job.", inputSchema:{type:"object",properties:{task_id:{type:"string"}},required:["task_id"],additionalProperties:false},handler:(input,context)=>job("TaskStop",input,context)},
  ];
  function job(name: string, raw: unknown, context: ToolContext) {
    authorize(context); context.signal.throwIfAborted();
    if (!raw || typeof raw!=="object" || Array.isArray(raw)) throw new Error("expected native job input");
    const input=raw as Record<string,unknown>;
    if (typeof input.task_id!=="string" || !input.task_id.startsWith("bash-")) throw new Error("native Bash task_id required; legacy Task IDs are not Bash jobs");
    const row=storage.sql.exec<{pin:string}>("SELECT pin FROM managed_claude_bash_jobs WHERE session_id=? AND task_id=?",context.sessionId,input.task_id).toArray()[0];
    if(!row) throw new Error("unknown native Bash job in this session; never resubmit uncertain work");
    return invoke(name,input,context,JSON.parse(row.pin));
  }
  return {tools, selected, reset, run:(input:Record<string,unknown>,context:ToolContext,workdir?:string)=>invoke("Bash",input,context,workdir===undefined?selected(context):undefined,workdir)};
}
