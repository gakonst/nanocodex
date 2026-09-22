/** User-facing parent-session controls never accept a caller-selected agent identity. */
export async function phoneControlInput(request: Request): Promise<Record<string, unknown>> {
  const url = new URL(request.url);
  if (url.search) throw new TypeError('unexpected query');
  if (url.pathname === '/phone/calls' && request.method === 'GET') return {operation:'list'};
  const match = url.pathname.match(/^\/phone\/calls\/([0-9a-f-]{36})\/(steer|hangup)$/);
  if (!match || request.method !== 'POST') throw new TypeError('invalid phone control');
  const reader = request.body?.getReader();
  let text = '', bytes = 0;
  const decoder = new TextDecoder('utf-8',{fatal:true,ignoreBOM:false});
  try {
    if (reader) for (;;) {
      const {done,value}=await reader.read(); if(done)break;
      bytes+=value.byteLength;
      if(bytes>32768){await reader.cancel();throw new TypeError('phone control too large');}
      text+=decoder.decode(value,{stream:true});
    }
    text+=decoder.decode();
    const value:unknown=JSON.parse(text || '{}');
    if(!value || typeof value!=='object' || Array.isArray(value))throw new TypeError('invalid phone control');
    const fields=match[2]==='steer'?['operation_id','instructions']:[];
    if(Object.keys(value).some(key=>!fields.includes(key)))throw new TypeError('unexpected phone control field');
    return {...value,operation:match[2],call_id:match[1]};
  } catch { throw new TypeError('invalid phone control'); }
  finally {reader?.releaseLock();}
}
