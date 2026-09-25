/** Bounded authenticated opt-in. Calendar/connection identity comes from the URL;
 * account/agent identity is supplied by the owning managed session. */
export async function calendarPushConfig(request: Request): Promise<{crm:true} | Response> {
  const reader=request.body?.getReader();
  if (!reader) return Response.json({error:"invalid_request"},{status:400});
  try {
    let length=0,text="";
    const decoder=new TextDecoder("utf-8",{fatal:true,ignoreBOM:false});
    while(true) {
      const {done,value}=await reader.read(); if(done) break;
      length+=value.byteLength;
      if(length>1024) {void reader.cancel().catch(()=>{});return Response.json({error:"request_too_large"},{status:413});}
      text+=decoder.decode(value,{stream:true});
    }
    const value:unknown=JSON.parse(text+decoder.decode());
    if (!value || typeof value!=="object" || Array.isArray(value) || Object.keys(value).length!==1 || !("crm" in value) || value.crm!==true) throw new Error();
    return {crm:true};
  } catch {return Response.json({error:"crm_opt_in_required"},{status:400});}
  finally {reader.releaseLock();}
}
