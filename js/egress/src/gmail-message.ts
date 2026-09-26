/** Bounded, text-only Gmail snapshots. Email content is always untrusted data. */
type GmailAttachmentReference = { filename?: string; mimeType?: string; size?: number; attachmentId?: string };
export type GmailMessageSnapshot = {
  id: string; status: "ok" | "missing" | "error" | "body_unavailable";
  headers?: Record<string, string>; body?: string; truncated?: boolean; attachments?: GmailAttachmentReference[];
};
const encoder = new TextEncoder();
export const jsonBytes = (value: unknown) => encoder.encode(JSON.stringify(value)).length;
const record = (v: any): v is Record<string, any> => !!v && typeof v === "object" && !Array.isArray(v);
function htmlText(html: string): string {
  return html.replace(/<!--[^]*?(?:-->|$)/g, "")
    .replace(/<(script|style|head)\b[^>]*>[^]*?(?:<\/\1\s*>|$)/gi, "")
    .replace(/<a\b([^>]*)>([^]*?)<\/a\s*>/gi, (_all, attributes: string, label: string) => {
      const match=/(?:^|\s)href\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+))/i.exec(attributes);
      const href=match?.[1]??match?.[2]??match?.[3];
      if(!href) return label;
      try {
        const url=new URL(href);
        return ["http:","https:","mailto:"].includes(url.protocol) ? `${label} (${url.href})` : label;
      } catch {return label;}
    })
    .replace(/<[^>]*>/g, " ")
    .replace(/&(#x[\da-f]+|#\d+|amp|lt|gt|quot|apos|nbsp);/gi, (all, entity: string) => {
      const names: Record<string,string> = {amp:"&",lt:"<",gt:">",quot:'"',apos:"'",nbsp:" "};
      if (entity[0] !== "#") return names[entity.toLowerCase()] ?? all;
      const n = entity[1].toLowerCase() === "x" ? parseInt(entity.slice(2),16) : Number(entity.slice(1));
      return n > 0 && n <= 0x10ffff && !(n >= 0xd800 && n <= 0xdfff) ? String.fromCodePoint(n) : "�";
    }).replace(/\s+/g," ").trim();
}
async function extract(payload: unknown, external: (id: string) => Promise<string>, signal: AbortSignal): Promise<{ body: string; truncated: boolean; available: boolean; attachments: GmailAttachmentReference[] }> {
  let nodes=0, truncated=false;
  const attachments: GmailAttachmentReference[]=[];
  async function visit(part: any, depth: number): Promise<{text:string; available:boolean}> {
    if (++nodes>200 || depth>20) {truncated=true; return {text:"",available:false};}
    if (!record(part)) throw new Error("invalid_mime");
    const headers=Array.isArray(part.headers)?part.headers:[];
    if (part.filename || headers.some((h:any)=>typeof h?.name==="string" && h.name.toLowerCase()==="content-disposition" && /^\s*attachment\b/i.test(h.value))) {
      if(attachments.length>=10) truncated=true;
      else {
        const reference:GmailAttachmentReference={};
        for(const [key,value,max] of [["filename",part.filename,512],["mimeType",part.mimeType,128],["attachmentId",part.body?.attachmentId,512]] as const) {
          if(typeof value==="string") {reference[key]=value.slice(0,max);if(value.length>max)truncated=true;}
        }
        if(Number.isSafeInteger(part.body?.size) && part.body.size>=0) reference.size=part.body.size;
        attachments.push(reference);
      }
      return {text:"",available:false};
    }
    const mime=String(part.mimeType??"").toLowerCase();
    if (mime.startsWith("multipart/") && Array.isArray(part.parts)) {
      let parts=part.parts;
      if (mime==="multipart/alternative") {
        const preferred = [...parts.filter((p:any)=>p?.mimeType==="text/plain"), ...parts.filter((p:any)=>p?.mimeType!=="text/plain").reverse()];
        let failure: unknown;
        for (const candidate of preferred.slice(0,200)) {
          try {
            const result=await visit(candidate,depth+1);
            if(result.available && result.text.trim()) return result;
          } catch (error) {signal.throwIfAborted();truncated=true;failure=error;}
        }
        if(failure) throw failure;
        return {text:"",available:false};
      }
      if (mime==="multipart/related") {
        const contentType=headers.find((h:any)=>typeof h?.name==="string" && h.name.toLowerCase()==="content-type")?.value??"";
        const start=/\bstart\s*=\s*(?:"([^"]+)"|([^;\s]+))/i.exec(contentType);
        const root=start?.[1]??start?.[2];
        parts=[(root && parts.find((p:any)=>Array.isArray(p?.headers) && p.headers.some((h:any)=>typeof h?.name==="string" && h.name.toLowerCase()==="content-id" && h.value===root))) || parts[0]].filter(Boolean);
      }
      const results=[]; for (const p of parts.slice(0,200)) results.push(await visit(p,depth+1));
      if(parts.length>200) truncated=true;
      return {text:results.map(r=>r.text).filter(Boolean).join("\n\n"),available:results.some(r=>r.available)};
    }
    if (mime!=="text/plain" && mime!=="text/html") return {text:"",available:false};
    const data=part.body?.attachmentId && !part.body?.data ? await external(part.body.attachmentId) : part.body?.data;
    if (typeof data!=="string" || !/^[A-Za-z0-9_+\/-]*={0,2}$/.test(data)) throw new Error("invalid_body");
    const bytes=Uint8Array.from(atob(data.replace(/-/g,"+").replace(/_/g,"/")),c=>c.charCodeAt(0));
    const contentType=headers.find((h:any)=>h?.name?.toLowerCase()==="content-type")?.value??"";
    const charset=/charset\s*=\s*["']?([^\s;"']+)/i.exec(contentType)?.[1]??"utf-8";
    const text=new TextDecoder(charset,{fatal:true,ignoreBOM:true}).decode(bytes);
    return {text:mime==="text/html"?htmlText(text):text,available:true};
  }
  const result=await visit(payload,0);
  return {body:result.text,truncated,available:result.available,attachments};
}
export async function hydrateGmailMessage(id:string, fetchMessage:(signal:AbortSignal, attachmentId?:string)=>Promise<Response>, budget:number, finalAttempt=false):Promise<GmailMessageSnapshot> {
  let retryable=false;
  const controller=new AbortController(); let reader:ReadableStreamDefaultReader<Uint8Array>|undefined;
  let timer:ReturnType<typeof setTimeout>;
  const timeout=new Promise<never>((_,reject)=>{timer=setTimeout(()=>{retryable=true;controller.abort(); void reader?.cancel().catch(()=>{});reject(new Error("timeout"));},2000);});
  try {
    return await Promise.race([timeout,(async()=>{
      let response:Response;
      try {response=await fetchMessage(controller.signal);} catch {retryable=true;throw new Error("transport");}
      if(controller.signal.aborted) {await response.body?.cancel();controller.signal.throwIfAborted();}
      if(response.status===404 || response.status===410) {await response.body?.cancel();return {id,status:"missing" as const};}
      if(!response.ok) {await response.body?.cancel();if(response.status===429 || response.status>=500){retryable=true;throw new Error("transient");}return {id,status:"error" as const};}
      const read = async (response: Response) => {
        if(controller.signal.aborted) {await response.body?.cancel();controller.signal.throwIfAborted();}
        reader=response.body?.getReader(); if(!reader) throw new Error("empty");
        let size=0;const chunks:Uint8Array[]=[];
        for(;;) {controller.signal.throwIfAborted();const chunk=await reader.read();controller.signal.throwIfAborted();if(chunk.done)break;size+=chunk.value.byteLength;if(size>1048576){await reader.cancel();throw new Error("oversize");}chunks.push(chunk.value);}
        const bytes=new Uint8Array(size);let offset=0;for(const chunk of chunks){bytes.set(chunk,offset);offset+=chunk.length;}
        return JSON.parse(new TextDecoder("utf-8",{fatal:true,ignoreBOM:true}).decode(bytes));
      };
      const raw=await read(response);
      if(!record(raw)||raw.id!==id||!record(raw.payload))throw new Error("invalid_message");
      let externalCount=0;
      const text=await extract(raw.payload, async attachmentId => {
        if (++externalCount>4 || !/^[A-Za-z0-9_-]{1,512}$/.test(attachmentId)) throw new Error("external_body_limit");
        controller.signal.throwIfAborted();
        const response=await fetchMessage(controller.signal,attachmentId);
        if(!response.ok) {await response.body?.cancel();retryable=response.status===429 || response.status>=500;throw new Error("external_body_unavailable");}
        const data=await read(response);return data.data;
      }, controller.signal);const headers:Record<string,string>={};let truncated=text.truncated;
      for(const h of Array.isArray(raw.payload.headers)?raw.payload.headers:[]) {
        if(typeof h?.name!=="string"||typeof h?.value!=="string")continue;
        const name=h.name.toLowerCase();if(!["from","to","cc","subject","date","message-id","reply-to"].includes(name)||name in headers)continue;
        headers[name]=h.value.slice(0,512);if(h.value.length>512)truncated=true;
      }
      const result:GmailMessageSnapshot={id,status:text.available?"ok":"body_unavailable",headers,body:text.body,truncated,...(text.attachments.length?{attachments:text.attachments}:{})};
      while(result.attachments?.length && jsonBytes(result.attachments)>Math.floor(budget/4)) {result.attachments.pop();result.truncated=true;}
      // Measure serialized UTF-8 including escapes, leaving space for the flag.
      while(jsonBytes(result)>budget && result.body) {result.truncated=true;result.body=result.body.slice(0,Math.max(0,Math.floor(result.body.length*.75)));}
      for(const key of Object.keys(headers).reverse()) {if(jsonBytes(result)<=budget)break;delete headers[key];result.truncated=true;}
      if (jsonBytes(result)>budget) {delete result.headers;delete result.body;delete result.attachments;result.truncated=true;}
      return result;
    })()]);
  } catch {if(retryable && !finalAttempt) throw new Error("gmail_body_retry");return {id,status:"error"};} finally {clearTimeout(timer!);}
}
