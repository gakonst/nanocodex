// Independently authored Node builtin adapters; filesystem authority stays in Rust.
(() => {
  'use strict';
  const alphabet='ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
  const encode=bytes=>{let out='';for(let i=0;i<bytes.length;i+=3){const a=bytes[i],b=bytes[i+1],c=bytes[i+2];out+=alphabet[a>>2]+alphabet[(a&3)<<4|(b??0)>>4]+(b===undefined?'=':alphabet[(b&15)<<2|(c??0)>>6])+(c===undefined?'=':alphabet[c&63]);}return out;};
  const decode=text=>{text=String(text).replace(/-/g,'+').replace(/_/g,'/').replace(/[^A-Za-z0-9+/=]/g,'');const bytes=[];let bits=0,value=0;for(const c of text){if(c==='=')break;const n=alphabet.indexOf(c);value=value<<6|n;bits+=6;if(bits>=8){bits-=8;bytes.push(value>>bits&255);}}return new Uint8Array(bytes);};
  const unwrap=raw=>{const packet=JSON.parse(raw);if(packet.error){const error=new Error(packet.error.message);Object.assign(error,packet.error);throw error;}return packet.result;};
  const fail=(message,code='ERR_INVALID_ARG_TYPE')=>{const error=new TypeError(message);error.code=code;throw error;};
  const view=value=>value instanceof ArrayBuffer?new Uint8Array(value):ArrayBuffer.isView(value)?new Uint8Array(value.buffer,value.byteOffset,value.byteLength):fail('Expected an ArrayBuffer or ArrayBufferView');
  class Buffer extends Uint8Array {
    static from(value,encoding='utf8',length){
      if(value instanceof ArrayBuffer)return new Buffer(value,typeof encoding==='number'?encoding:0,length);
      if(typeof value==='string'){
        encoding=String(encoding).toLowerCase();let bytes;
        if(encoding==='utf8'||encoding==='utf-8')bytes=new TextEncoder().encode(value);
        else if(encoding==='base64'||encoding==='base64url')bytes=decode(value);
        else if(encoding==='hex'){bytes=[];for(let i=0;i+1<value.length&&/^[0-9a-f]{2}$/i.test(value.slice(i,i+2));i+=2)bytes.push(parseInt(value.slice(i,i+2),16));}
        else if(['latin1','binary','ascii'].includes(encoding))bytes=Array.from({length:value.length},(_,i)=>value.charCodeAt(i)&255);
        else if(['utf16le','utf-16le','ucs2','ucs-2'].includes(encoding)){bytes=[];for(let i=0;i<value.length;i++){const point=value.charCodeAt(i);bytes.push(point&255,point>>8);}}
        else fail('Unknown encoding: '+encoding,'ERR_UNKNOWN_ENCODING');
        return new Buffer(bytes);
      }
      if(ArrayBuffer.isView(value)||Array.isArray(value))return new Buffer(value);
      if(value?.type==='Buffer'&&Array.isArray(value.data))return new Buffer(value.data);
      return fail('Buffer.from requires a string, Buffer, ArrayBuffer or array');
    }
    static alloc(size,fill=0,encoding){if(!Number.isInteger(size)||size<0||size>8*1024*1024)fail('Invalid Buffer size','ERR_OUT_OF_RANGE');const buffer=new Buffer(size);if(typeof fill==='string'){const bytes=Buffer.from(fill,encoding);if(bytes.length)for(let i=0;i<size;i++)buffer[i]=bytes[i%bytes.length];}else buffer.fill(fill);return buffer;}
    static allocUnsafe(size){return Buffer.alloc(size);}
    static isBuffer(value){return value instanceof Buffer;}
    static isEncoding(value){return ['utf8','utf-8','ascii','latin1','binary','base64','base64url','hex','utf16le','utf-16le','ucs2','ucs-2'].includes(String(value).toLowerCase());}
    static byteLength(value,encoding){return typeof value==='string'?Buffer.from(value,encoding).length:view(value).byteLength;}
    static concat(list,totalLength=list.reduce((size,value)=>size+value.length,0)){const out=Buffer.alloc(totalLength);let offset=0;for(const value of list){const bytes=view(value).subarray(0,Math.max(0,totalLength-offset));out.set(bytes,offset);offset+=bytes.length;}return out;}
    static compare(a,b){for(let i=0;i<Math.min(a.length,b.length);i++)if(a[i]!==b[i])return a[i]<b[i]?-1:1;return Math.sign(a.length-b.length);}
    toString(encoding='utf8',start=0,end=this.length){return unwrap(__skyre_codec(JSON.stringify({data:encode(this.subarray(Math.max(0,start),Math.max(0,end))),encoding})));}
    toJSON(){return {type:'Buffer',data:Array.from(this)};}
    equals(other){return Buffer.compare(this,other)===0;}
    compare(other){return Buffer.compare(this,other);}
    slice(start,end){return this.subarray(start,end);}
    copy(target,targetStart=0,sourceStart=0,sourceEnd=this.length){const bytes=this.subarray(sourceStart,Math.min(sourceEnd,sourceStart+target.length-targetStart));target.set(bytes,targetStart);return bytes.length;}
    write(text,offset=0,length=this.length-offset,encoding='utf8'){if(typeof offset==='string'){encoding=offset;offset=0;length=this.length;}else if(typeof length==='string'){encoding=length;length=this.length-offset;}const bytes=Buffer.from(text,encoding).subarray(0,Math.min(length,this.length-offset));this.set(bytes,offset);return bytes.length;}
  }
  // Numeric methods use byte-indexed access so Uint8Array and generic receivers
  // retain the installed methods' validation order and observable writes.
  const inspectedNumber=value=>{
    let text=String(value);
    if(typeof value==='bigint')return (value>0x100000000n||value< -0x100000000n?text.replace(/\B(?=(\d{3})+(?!\d))/g,'_'):text)+'n';
    if(typeof value==='number'&&Number.isInteger(value)&&Math.abs(value)>0x100000000)text=(text.startsWith('-')?'-':'')+text.replace(/^-/,'').replace(/(.)(?=(.{3})+$)/g,'$1_');
    return text;
  };
  const received=value=>value===null?'null':value===undefined?'undefined':typeof value==='object'?'an instance of '+(value.constructor?.name||'Object'):typeof value==='function'?'function '+(value.name||''):typeof value==='string'?"type string ("+JSON.stringify(value).replace(/^"|"$/g,"'")+")":'type '+typeof value+' ('+inspectedNumber(value)+')';
  const typedError=(name,expected,value)=>{const error=new TypeError('The "'+name+'" argument must be '+expected+'. Received '+received(value));error.code='ERR_INVALID_ARG_TYPE';throw error;};
  const outOfRange=(name,range,value)=>{const error=new RangeError('The value of "'+name+'" is out of range. It must be '+range+'. Received '+inspectedNumber(value));error.code='ERR_OUT_OF_RANGE';throw error;};
  const numericType=(value,name)=>{if(typeof value!=='number')typedError(name,'of type number',value);};
  const badBounds=(value,max,name='offset')=>{
    if(Math.floor(numericValue(value))!==value){numericType(value,name);outOfRange(name,'an integer',value);}
    if(max<0){const error=new RangeError('Attempt to access memory outside buffer bounds');error.code='ERR_BUFFER_OUT_OF_BOUNDS';throw error;}
    outOfRange(name,'>= '+(name==='byteLength'?1:0)+' and <= '+max,value);
  };
  const numericBounds=(buffer,offset,bytes)=>{numericType(offset,'offset');if(buffer===null||buffer===undefined)throw new TypeError("Cannot read properties of "+String(buffer)+" (reading '"+offset+"')");if(buffer[offset]===undefined||buffer[offset+bytes-1]===undefined)badBounds(offset,buffer.length-bytes);};
  const primitive=value=>{if(value===null||typeof value!=='object'&&typeof value!=='function')return value;const exotic=value[Symbol.toPrimitive];if(exotic!=null){if(typeof exotic!=='function')throw new TypeError('Cannot convert object to primitive value');const result=exotic.call(value,'number');if(result===null||typeof result!=='object'&&typeof result!=='function')return result;throw new TypeError('Cannot convert object to primitive value');}for(const name of ['valueOf','toString']){const method=value[name];if(typeof method==='function'){const result=method.call(value);if(result===null||typeof result!=='object'&&typeof result!=='function')return result;}}throw new TypeError('Cannot convert object to primitive value');};
  const numericValue=input=>{const value=primitive(input);if(typeof value==='bigint')throw new TypeError('Cannot convert a BigInt value to a number');if(typeof value==='symbol')throw new TypeError('Cannot convert a Symbol value to a number');return +value;};
  const variableWidth=(offset,width,reading)=>{if(reading&&offset===undefined)typedError('offset','of type number',offset);if(![1,2,3,4,5,6].includes(width))badBounds(width,6,'byteLength');};
  const integerRange=(value,bytes,signed,big)=>{
    const bits=bytes*8,limit=big?1n<<BigInt(bits-(signed?1:0)):2**(bits-(signed?1:0));
    const min=signed?-limit:big?0n:0,max=limit-(big?1n:1);
    if(typeof value==='symbol')throw new TypeError('Cannot convert a Symbol value to a number');
    const compare=upper=>{const operand=big?primitive(value):value,bound=upper?max:min;if(typeof operand==='symbol')throw new TypeError('Cannot convert a Symbol value to a number');if(big&&typeof operand==='number'){if(Number.isInteger(operand))return upper?BigInt(operand)>bound:BigInt(operand)<bound;return upper?operand>Number(bound):operand<Number(bound);}return upper?operand>bound:operand<bound;};
    const upper=compare(true),lower=!upper&&compare(false);
    if(lower||upper){const n=big?'n':'';const range=bytes>4?(signed?'>= -(2'+n+' ** '+(bits-1)+n+') and < 2'+n+' ** '+(bits-1)+n:'>= 0'+n+' and < 2'+n+' ** '+bits+n):'>= '+min+n+' and <= '+max+n;outOfRange('value',range,value);}
  };
  const readInteger=(buffer,offset,bytes,signed,little,big)=>{
    numericBounds(buffer,offset,bytes);let value=big?0n:0;
    for(let i=0;i<bytes;i++){const byte=buffer[offset+(little?bytes-i-1:i)];value=big?(value<<8n)+BigInt(byte):value*256+byte;}
    if(signed){const bits=bytes*8;if(big?value>=(1n<<BigInt(bits-1)):value>=2**(bits-1))value-=big?1n<<BigInt(bits):2**bits;}
    return value;
  };
  const writeInteger=(buffer,input,offset,bytes,signed,little,big)=>{
    let value=big?input:numericValue(input);if(bytes===1)numericType(offset,'offset');integerRange(value,bytes,signed,big);numericBounds(buffer,offset,bytes);
    if(big){
      const operand=()=>{const result=primitive(value);if(typeof result==='symbol')throw new TypeError('Cannot convert a Symbol value to a number');if(typeof result!=='bigint')throw new TypeError('Cannot mix BigInt and other types, use explicit conversions');return result;};
      let low=Number(operand()&0xffffffffn);for(let i=0;i<4;i++){buffer[offset+(little?i:7-i)]=low&255;low>>>=8;}
      let high=Number((operand()>>32n)&0xffffffffn);for(let i=0;i<4;i++){buffer[offset+(little?i+4:3-i)]=high&255;high>>>=8;}
      return offset+8;
    }
    value=Math.trunc(value)||0;if(value<0)value+=2**(bytes*8);
    for(let i=0;i<bytes;i++){buffer[offset+(little?i:bytes-i-1)]=big?Number(value&255n):value%256;value=big?value>>8n:Math.floor(value/256);}
    return offset+bytes;
  };
  const numericFunctions=new WeakSet();
  const method=(name,fn)=>{if(!numericFunctions.has(fn)){numericFunctions.add(fn);const fixedRead=/^read(?:UInt|Int)(?:8|16|32)(?:LE|BE)?$/.test(name);Object.defineProperty(fn,'name',{value:fixedRead?'':name.replace(/(Float|Double)(LE|BE)$/,(_,kind,endian)=>kind+(endian==='LE'?'Forwards':'Backwards')),configurable:true});if(fixedRead)Object.defineProperty(fn,'length',{value:1,configurable:true});}Object.defineProperty(Buffer.prototype,name,{value:fn,writable:true,configurable:true,enumerable:true});};
  for(const signed of [false,true])for(const bytes of [1,2,4,8])for(const little of bytes===1?[true]:[true,false]){
    const big=bytes===8,kind=big?(signed?'BigInt':'BigUInt'):(signed?'Int':'UInt'),suffix=bytes*8+(bytes===1?'':little?'LE':'BE');
    method('read'+kind+suffix,function(offset=0){return readInteger(this,offset,bytes,signed,little,big);});
    method('write'+kind+suffix,function(value,offset=0){return writeInteger(this,value,offset,bytes,signed,little,big);});
  }
  for(const signed of [false,true])for(const little of [false,true]){
    const suffix=(signed?'Int':'UInt')+(little?'LE':'BE');
    method('read'+suffix,function(offset,byteLength){variableWidth(offset,byteLength,true);return readInteger(this,offset,byteLength,signed,little,false);});
    method('write'+suffix,function(value,offset,byteLength){variableWidth(offset,byteLength,false);return writeInteger(this,value,offset,byteLength,signed,little,false);});
  }
  for(const bytes of [4,8])for(const little of [false,true]){
    const suffix=(bytes===4?'Float':'Double')+(little?'LE':'BE'),floatMethod='Float'+bytes*8;
    method('read'+suffix,function(offset=0){numericBounds(this,offset,bytes);const data=new Uint8Array(bytes);for(let i=0;i<bytes;i++)data[i]=this[offset+i];return new DataView(data.buffer)['get'+floatMethod](0,little);});
    // Preserve numeric NaN sign/payloads, but canonicalize ToNumber(undefined).
    // V8 can otherwise expose its internal undefined-NaN payload through DataView.
    method('write'+suffix,function(input,offset=0){const converted=primitive(input),value=converted===undefined?NaN:numericValue(converted);numericBounds(this,offset,bytes);const data=new Uint8Array(bytes);new DataView(data.buffer)['set'+floatMethod](0,value,little);for(let i=0;i<bytes;i++)this[offset+i]=data[i];return offset+bytes;});
  }
  for(const name of Object.getOwnPropertyNames(Buffer.prototype))if(name.includes('UInt'))method(name.replace('UInt','Uint'),Buffer.prototype[name]);
  // Buffer search and mutation overloads are validated before touching bytes.
  const shortInspect=value=>typeof value==='string'?JSON.stringify(value).replace(/^"|"$/g,"'"):value===null?'null':typeof value==='object'?(Buffer.isBuffer(value)?'<Buffer '+Array.from(value,b=>b.toString(16).padStart(2,'0')).join(' ')+'>':Array.isArray(value)?'[]':'{}'):String(value);
  const bufferEncoding=value=>{if(value===undefined)return 'utf8';if(typeof value!=='string')return undefined;const lower=value.toLowerCase();if(['utf8','utf-8'].includes(lower))return 'utf8';if(['utf16le','utf-16le','ucs2','ucs-2'].includes(lower))return 'utf16le';if(['latin1','binary'].includes(lower))return 'latin1';return ['ascii','hex','base64','base64url'].includes(lower)?lower:undefined;};
  const unknownEncoding=value=>fail('Unknown encoding: '+shortInspect(value).replace(/^'(.*)'$/,'$1'),'ERR_UNKNOWN_ENCODING');
  const offsetValue=(value,name,max=Number.MAX_SAFE_INTEGER)=>{numericType(value,name);if(!Number.isInteger(value))outOfRange(name,'an integer',value);if(value<0||value>max)outOfRange(name,'>= 0 && <= '+max,value);return value;};
  const intrinsicArray=Object.getPrototypeOf(Uint8Array.prototype),arrayLength=Object.getOwnPropertyDescriptor(intrinsicArray,'length').get;
  const arraySize=(buffer,property='length')=>{try{return property==='length'?arrayLength.call(buffer):Object.getOwnPropertyDescriptor(intrinsicArray,'byteLength').get.call(buffer);}catch{throw new TypeError('Method get TypedArray.prototype.'+property+' called on incompatible receiver '+String(buffer));}};
  const byteArray=value=>value instanceof Uint8Array;
  const searchBuffer=(buffer,value,offset,encoding,forward)=>{
    if(!ArrayBuffer.isView(buffer))typedError('buffer','an instance of Buffer, TypedArray, or DataView',buffer);
    buffer=new Uint8Array(buffer.buffer,buffer.byteOffset,buffer.byteLength);
    if(typeof offset==='symbol')throw new TypeError('Cannot convert a Symbol value to a number');
    if(typeof offset==='string'){encoding=offset;offset=undefined;}
    else {const compare=()=>{const value=primitive(offset);if(typeof value==='symbol')throw new TypeError('Cannot convert a Symbol value to a number');return value;};if(compare()>0x7fffffff)offset=0x7fffffff;else if(compare()< -0x80000000)offset=-0x80000000;}
    offset=numericValue(offset);if(Number.isNaN(offset))offset=forward?0:buffer.length;
    offset=Math.trunc(offset);let length=buffer.length,needle,unit=1;const stringNeedle=typeof value==='string';
    if(typeof value==='number')needle=Uint8Array.of(value>>>0&255);
    else {
      const enc=bufferEncoding(encoding);
      if(typeof value==='string'){if(!enc)unknownEncoding(encoding);needle=Buffer.from(value,enc);}
      else if(byteArray(value))needle=value;
      else typedError('value','one of type number or string or an instance of Buffer or Uint8Array',value);
      if(enc==='utf16le'){unit=2;if(stringNeedle)length-=length%2;}
    }
    if(offset<0)offset+=length;
    if(needle.length===0)return Math.min(length,Math.max(0,offset));
    if(forward){if(offset<0)offset=0;if(offset>=length)return -1;}else{if(offset<0)return -1;offset=Math.min(offset,length-1);}
    if(needle.length>length||(forward&&offset+needle.length>length))return -1;
    const end=Math.floor(length/unit),size=Math.floor(needle.length/unit);if(!size||end<size)return -1;
    let at=Math.floor(offset/unit);if(forward&&at+size>end)return -1;if(!forward)at=Math.min(at,end-size);
    if(size>=8&&end>=4096){
      // Linear matching avoids repeated scans for long overlapping binary patterns.
      const read=(bytes,index)=>unit===1?bytes[index]:bytes[index*2]|bytes[index*2+1]<<8;
      const pattern=index=>read(needle,forward?index:size-1-index),prefix=new Uint32Array(size);
      for(let i=1,j=0;i<size;i++){while(j&&pattern(i)!==pattern(j))j=prefix[j-1];if(pattern(i)===pattern(j))j++;prefix[i]=j;}
      for(let i=forward?at:at+size-1,j=0;forward?i<end:i>=0;i+=forward?1:-1){const value=read(buffer,i);while(j&&value!==pattern(j))j=prefix[j-1];if(value===pattern(j))j++;if(j===size)return (forward?i-size+1:i)*unit;}
    }else for(;forward?at+size<=end:at>=0;at+=forward?1:-1){let match=true;for(let j=0;j<size*unit;j++)if(buffer[at*unit+j]!==needle[j]){match=false;break;}if(match)return at*unit;}
    // The installed native Buffer UCS2 search compares its even sentinel to the raw byte length.
    return unit===2&&!stringNeedle&&length%2?end*2:-1;
  };
  method('indexOf',function indexOf(value,byteOffset,encoding){return searchBuffer(this,value,byteOffset,encoding,true);});
  method('lastIndexOf',function lastIndexOf(value,byteOffset,encoding){return searchBuffer(this,value,byteOffset,encoding,false);});
  method('includes',function includes(value,byteOffset,encoding){return searchBuffer(this,value,byteOffset,encoding,true)!==-1;});
  for(const size of [2,4,8])method('swap'+size*8,function(){const length=arraySize(this);if(length%size){const error=new RangeError('Buffer size must be a multiple of '+size*8+'-bits');error.code='ERR_INVALID_BUFFER_SIZE';throw error;}for(let i=0;i<length;i+=size)for(let j=0;j<size/2;j++){const byte=this[i+j];this[i+j]=this[i+size-j-1];this[i+size-j-1]=byte;}return this;});
  method('fill',function fill(value,offset,end,encoding){
    let enc;
    if(typeof value==='string'){
      if(offset===undefined||typeof offset==='string'){encoding=offset;offset=0;if(this===null||this===undefined)throw new TypeError("Cannot read properties of "+String(this)+" (reading 'length')");end=this.length;}
      else if(typeof end==='string'){encoding=end;end=this.length;}
      enc=bufferEncoding(encoding==null||encoding===''?'utf8':encoding);if(!enc){if(typeof encoding!=='string')typedError('encoding','of type string',encoding);unknownEncoding(encoding);}
      if(value==='')value=0;
    }
    if(offset===undefined){offset=0;end=this.length;}else{offsetValue(offset,'offset');if(end===undefined)end=this.length;else offsetValue(end,'end',this.length);if(offset>=end)return this;}
    arraySize(this,'byteLength');let pattern;
    if(typeof value==='string')pattern=Buffer.from(value,enc);
    else if(ArrayBuffer.isView(value))pattern=new Uint8Array(value.buffer,value.byteOffset,value.byteLength).slice();
    else pattern=Uint8Array.of(numericValue(value)&255);
    if(!pattern.length)fail("The argument 'value' is invalid. Received "+shortInspect(value),'ERR_INVALID_ARG_VALUE');
    for(let i=offset;i<end;i++)this[i]=pattern[(i-offset)%pattern.length];return this;
  });
  method('write',function write(string,offset,length,encoding){
    const size=arraySize(this);
    if(offset===undefined){offset=0;length=size;encoding='utf8';}
    else if(length===undefined&&typeof offset==='string'){encoding=offset;offset=0;length=size;}
    else{offsetValue(offset,'offset',size);const remaining=size-offset;if(length===undefined)length=remaining;else if(typeof length==='string'){encoding=length;length=remaining;}else{offsetValue(length,'length',size);length=Math.min(length,remaining);}}
    const enc=bufferEncoding(encoding||'utf8');if(!enc)unknownEncoding(encoding);
    if(typeof string!=='string')fail('argument must be a string');
    let bytes;
    if(enc==='utf8'){
      const out=[];for(const point of string){const part=utf8(point);if(out.length+part.length>length)break;out.push(...part);}bytes=out;
    }else {bytes=Buffer.from(string,enc);length=Math.min(length,bytes.length);if(enc==='utf16le')length-=length%2;bytes=bytes.subarray(0,length);}
    for(let i=0;i<bytes.length;i++)this[offset+i]=bytes[i];return bytes.length;
  });
  globalThis.Buffer=Buffer;
  const decoderStates=new WeakMap();
  const decoderState=value=>{const state=decoderStates.get(value);if(!state)fail('Value of "this" must be of type TextDecoder','ERR_INVALID_THIS');return state;};
  const decoderOptions=value=>{if(value!==null&&typeof value!=='object')typedError('options','of type object',value);};
  function TextDecoder(encoding='utf-8',options={}) {
    if(!new.target)throw new TypeError("Class constructor TextDecoder cannot be invoked without 'new'");
    if(typeof encoding==='symbol')throw new TypeError('Cannot convert a Symbol value to a string');
    encoding=String(encoding);decoderOptions(options);
    const canonical=unwrap(__skyre_codec(JSON.stringify({operation:'decoder_label',encoding})));
    if(canonical.unsupported){const error=new RangeError('The "'+(canonical.encoding??encoding)+'" encoding is not supported');error.code='ERR_ENCODING_NOT_SUPPORTED';throw error;}
    const state={encoding:canonical.encoding,fatal:!!options?.fatal,ignoreBOM:!!options?.ignoreBOM,id:null,bomSeen:false};
    // The installed constructor reads option accessors twice, in this order.
    state.utf8IgnoreBOM=!!options?.ignoreBOM;state.utf8Fatal=!!options?.fatal;
    const target=this,proxy=new Proxy(target,{set(object,key,value,receiver){if(['encoding','fatal','ignoreBOM'].includes(key)&&!Object.hasOwn(object,key))throw new TypeError('Cannot set property '+key+' of [object TextDecoder] which has only a getter');return Reflect.set(object,key,value,receiver);}});
    decoderStates.set(target,state);decoderStates.set(proxy,state);return proxy;
  }
  Object.defineProperty(TextDecoder.prototype,'decode',{enumerable:true,configurable:true,writable:true,value:function decode(input=new Uint8Array(),options={}){
    const state=decoderState(this);decoderOptions(options);const single=!['utf-8','utf-16le','utf-16be','big5','shift_jis','euc-jp','euc-kr','gbk','gb18030','iso-2022-jp'].includes(state.encoding);const stream=single?false:!!options?.stream;const omitBom=state.encoding==='utf-8'&&!state.utf8IgnoreBOM&&!state.bomSeen;if(!stream)state.bomSeen=false;
    let bytes;
    if(input instanceof ArrayBuffer)bytes=new Uint8Array(input);
    else if(ArrayBuffer.isView(input))bytes=new Uint8Array(input.buffer,input.byteOffset,input.byteLength);
    else if(state.encoding==='utf-8'&&!stream&&!state.id)fail('The "list" argument must be an instance of SharedArrayBuffer, ArrayBuffer or ArrayBufferView.');
    else if(!single&&state.encoding!=='utf-8')fail('The "input" argument must be an instance of SharedArrayBuffer, ArrayBuffer or ArrayBufferView.');
    else typedError('input','an instance of ArrayBuffer or ArrayBufferView',input);
    const result=unwrap(__skyre_codec(JSON.stringify({operation:'decoder_decode',encoding:state.encoding,fatal:state.encoding==='utf-8'?state.utf8Fatal:state.fatal,ignoreBOM:state.encoding==='utf-8'?true:state.ignoreBOM,id:state.id,data:encode(bytes),stream})));
    state.id=result.id;
    if(result.invalid)fail('The encoded data was not valid for encoding '+(state.encoding==='iso-8859-8-i'?'iso-8859-8':state.encoding),'ERR_ENCODING_INVALID_ENCODED_DATA');
    if(state.encoding==='utf-8'){if(stream&&result.text.length)state.bomSeen=true;if(omitBom&&result.text.charCodeAt(0)===0xfeff)return result.text.slice(1);}
    return result.text;
  }});
  const decoderGetters={get encoding(){return decoderState(this).encoding;},get fatal(){return decoderState(this).fatal;},get ignoreBOM(){return decoderState(this).ignoreBOM;}};
  Object.defineProperties(TextDecoder.prototype,Object.getOwnPropertyDescriptors(decoderGetters));
  Object.defineProperty(TextDecoder.prototype,Symbol.toStringTag,{value:'TextDecoder',configurable:true});
  globalThis.TextDecoder=TextDecoder;
  const pathString=value=>typeof value==='string'?value:fail('The path argument must be of type string');
  function makePath(windows){
    const sep=windows?'\\':'/',split=value=>value.split(windows?/[\\/]+/:/\/+/);
    const parseRoot=value=>{
      let normalized=windows?value.replace(/\//g,'\\'):value,device='',absolute=false,end=0;
      if(windows&&/^[A-Za-z]:/.test(normalized)){device=normalized.slice(0,2);end=2;if(normalized[2]==='\\'){absolute=true;end=3;}}
      else if(windows&&normalized.startsWith('\\\\')){const match=/^\\\\([^\\]+)\\([^\\]+)(?:\\|$)/.exec(normalized);if(match){device='\\\\'+match[1]+'\\'+match[2];absolute=true;end=match[0].length;}else{absolute=true;end=1;}}
      else if(normalized.startsWith(sep)){absolute=true;end=1;}
      return {normalized,device,absolute,end,root:value.slice(0,end),tail:normalized.slice(end)};
    };
    const compact=(parts,absolute)=>{const out=[];for(const part of parts){if(!part||part==='.')continue;if(part==='..'&&out.length&&out.at(-1)!=='..')out.pop();else if(part!=='..'||!absolute)out.push(part);}return out;};
    const api={sep,delimiter:windows?';':':',
      isAbsolute(value){return parseRoot(pathString(value)).absolute;},
      normalize(value){value=pathString(value);if(!value)return '.';const root=parseRoot(value),parts=compact(split(root.tail),root.absolute);let result=root.device+(root.absolute?sep:'')+parts.join(sep);if(!result)result='.';else if(root.device&&!root.absolute&&!parts.length)result+='.';if(/[\\/]$/.test(value)&&(windows||value.endsWith('/'))&&!result.endsWith(sep))result+=sep;return result;},
      join(...values){values.forEach(pathString);return api.normalize(values.filter(Boolean).join(sep));},
      resolve(...values){
        values.forEach(pathString);let device='',absolute=false,tail='';
        for(let i=values.length-1;i>=-1;i--){let value=i>=0?values[i]:nodeRepl.cwd||sep;if(!value)continue;const root=parseRoot(value);
          if(root.device){if(device&&root.device.toLowerCase()!==device.toLowerCase())continue;device=root.device;}
          if(!absolute){tail=root.tail+sep+tail;absolute=root.absolute;}
          if(absolute&&(!windows||device))break;
        }
        const parts=compact(split(tail),absolute);return device+(absolute?sep:'')+parts.join(sep)||'.';
      },
      basename(value,suffix){value=pathString(value);if(suffix!==undefined)pathString(suffix);let base=(windows&&/^[A-Za-z]:/.test(value)?value.slice(2):value).replace(windows?/[\\/]+$/:/\/+$/,'');base=base.slice(Math.max(base.lastIndexOf('/'),windows?base.lastIndexOf('\\'):-1)+1);if(suffix&&base.endsWith(suffix))base=base.slice(0,-suffix.length);return base;},
      dirname(value){value=pathString(value);if(!value)return '.';const root=parseRoot(value);let end=value.length;while(end>root.end&&(value[end-1]==='/'||(windows&&value[end-1]==='\\')))end--;let index=end-1;while(index>=root.end&&value[index]!=='/'&&(!windows||value[index]!=='\\'))index--;if(index<root.end)return root.root||'.';if(!windows&&index===1&&value.startsWith('//'))return '//';return value.slice(0,index);},
      extname(value){const base=api.basename(value),dot=base.lastIndexOf('.');return dot<=0||base==='..'?'':base.slice(dot);},
      parse(value){value=pathString(value);const info=parseRoot(value),root=info.root;let end=value.length;while(end>info.end&&(value[end-1]==='/'||(windows&&value[end-1]==='\\')))end--;let index=end-1;while(index>=info.end&&value[index]!=='/'&&(!windows||value[index]!=='\\'))index--;const base=value.slice(Math.max(info.end,index+1),end),dot=base.lastIndexOf('.'),ext=dot<=0||base==='..'?'':base.slice(dot),dir=index<info.end?root:value.slice(0,index);return {root,dir,base,ext,name:base.slice(0,base.length-ext.length)};},
      format(value){if(!value||typeof value!=='object')fail('The pathObject argument must be an object');const dir=value.dir||value.root||'',base=value.base||(value.name||'')+(value.ext?(value.ext.startsWith('.')?value.ext:'.'+value.ext):'');return !dir?base:dir===value.root?dir+base:dir+sep+base;},
      relative(from,to){from=api.resolve(from);to=api.resolve(to);if((windows?from.toLowerCase():from)===(windows?to.toLowerCase():to))return '';const a=parseRoot(from),b=parseRoot(to);if(windows&&a.device.toLowerCase()!==b.device.toLowerCase())return to;const left=split(a.tail).filter(Boolean),right=split(b.tail).filter(Boolean);let common=0;while(common<Math.min(left.length,right.length)&&(windows?left[common].toLowerCase()===right[common].toLowerCase():left[common]===right[common]))common++;return Array(left.length-common).fill('..').concat(right.slice(common)).join(sep);},
      toNamespacedPath(value){if(!windows||typeof value!=='string'||!value)return value;const resolved=api.resolve(value);if(resolved.startsWith('\\\\')&&!resolved.startsWith('\\\\?\\')&&!resolved.startsWith('\\\\.\\'))return '\\\\?\\UNC\\'+resolved.slice(2);if(/^[A-Za-z]:\\/.test(resolved))return '\\\\?\\'+resolved;return value;},
    };return api;
  }
  const posix=makePath(false),win32=makePath(true);posix.posix=win32.posix=posix;posix.win32=win32.win32=win32;
  const path=__skyre_runtime_platform==='windows'?win32:posix;
  const NativeURL=globalThis.URL,NativeURLSearchParams=globalThis.URLSearchParams;
  const urlError=(code,message,kind=TypeError)=>{const error=new kind(message);error.code=code;throw error;};
  const parseURL=value=>{try{return new NativeURL(value);}catch{urlError('ERR_INVALID_URL','Invalid URL');}};
  const isURL=value=>{try{Object.getOwnPropertyDescriptor(NativeURL.prototype,'href').get.call(value);return true;}catch{return false;}};
  const domain=(name,value)=>{if(typeof value==='symbol')throw new TypeError('Cannot convert a Symbol value to a string');return unwrap(__skyre_codec(JSON.stringify({operation:name,domain:''+value})));};
  const utf8=value=>new TextEncoder().encode(value);
  const rawPercentDecode=text=>{const output=[];for(let i=0;i<text.length;){if(text[i]==='%'&&/^[0-9a-f]{2}$/i.test(text.slice(i+1,i+3))){output.push(parseInt(text.slice(i+1,i+3),16));i+=3;}else{const point=String.fromCodePoint(text.codePointAt(i));output.push(...utf8(point));i+=point.length;}}return Buffer.from(output);};
  const filePath=(input,options,asBuffer)=>{
    const value=typeof input==='string'?parseURL(input):isURL(input)?input:typedError('path','of type string or an instance of URL',input);
    if(value.protocol!=='file:')urlError('ERR_INVALID_URL_SCHEME','The URL must be of scheme file');
    const windows=options?.windows??(__skyre_runtime_platform==='windows');
    const pathname=value.pathname,host=value.hostname;
    if(!asBuffer&&(windows?/%(?:2f|5c)/i.test(pathname):/%2f/i.test(pathname)))urlError('ERR_INVALID_FILE_URL_PATH','File URL path must not include encoded '+(windows?'\\ or /':'/')+' characters');
    if(!windows&&host)urlError('ERR_INVALID_FILE_URL_HOST','File URL host must be "localhost" or empty on '+(__skyre_runtime_platform==='mac'?'darwin':__skyre_runtime_platform==='windows'?'win32':'linux'));
    let text=windows?pathname.replace(/\//g,'\\'):pathname;
    let data;
    if(asBuffer)data=rawPercentDecode(text);
    else {try{data=decodeURIComponent(text);}catch{throw new URIError('URI malformed');}}
    if(windows){
      if(host){const prefix='\\\\'+domain('domainToUnicode',host);data=asBuffer?Buffer.concat([Buffer.from(prefix),data]):prefix+data;}
      else {const drive=asBuffer?String.fromCharCode(...data.subarray(0,3)):data.slice(0,3);if(!/^\\[A-Za-z]:$/.test(drive))urlError('ERR_INVALID_FILE_URL_PATH','File URL path must be absolute');data=asBuffer?data.subarray(1):data.slice(1);}
    }
    return data;
  };
  const encodeFilePath=value=>{let output='';for(const point of value){const n=point.codePointAt(0);if(n<=32||n>=127||'%#?\\'.includes(point))for(const byte of utf8(point))output+='%'+byte.toString(16).toUpperCase().padStart(2,'0');else output+=point;}return output;};
  const stringQuery=query=>{
    const parts=[];for(const key of Object.keys(query)){const values=Array.isArray(query[key])?query[key]:[query[key]];for(const value of values){const text=typeof value==='string'||typeof value==='boolean'||typeof value==='number'&&Number.isFinite(value)||typeof value==='bigint'?String(value):'';parts.push(encodeURIComponent(key)+'='+encodeURIComponent(text));}}return parts.join('&');
  };
  const legacyFormat=value=>{
    if(typeof value==='string'){
      let text=value.trim();const separator=text.search(/[?#]/),prefix=separator<0?text:text.slice(0,separator);text=prefix.replace(/\\/g,'/')+(separator<0?'':text.slice(separator));
      const match=/^([A-Za-z0-9.+-]+:)/.exec(text),protocol=match?match[1].toLowerCase():'';
      let rest=match?text.slice(match[0].length):text,authority='',slashes=false;
      const special=['http:','https:','ftp:','gopher:','file:','ws:','wss:'].includes(protocol);
      if(rest.startsWith('//')&&protocol){slashes=true;rest=rest.slice(2);}
      if(protocol&&(slashes||!special)&&protocol!=='javascript:'){
        const end=rest.search(/[/?#]/);authority=end<0?rest:rest.slice(0,end);rest=end<0?'':rest.slice(end);
      }
      let host='',auth='';
      if(authority){
        const at=authority.lastIndexOf('@');if(at>=0){auth=decodeURIComponent(authority.slice(0,at));authority=authority.slice(at+1);}
        const port=/(?::([0-9]*))$/.exec(authority),hasPort=port&&(!authority.startsWith('[')||authority.includes(']:'));
        const hostname=hasPort?authority.slice(0,port.index):authority;
        if(hostname){try{host=new NativeURL('http://'+hostname).hostname;}catch{urlError('ERR_INVALID_URL','Invalid URL');}}
        if(hasPort&&port[1])host+=':'+port[1];
      }
      if(protocol!=='javascript:')rest=rest.replace(/[<>"` \r\n\t{}|^']/g,c=>'%'+c.charCodeAt(0).toString(16).toUpperCase().padStart(2,'0'));
      const hashIndex=rest.indexOf('#'),hash=hashIndex<0?'':rest.slice(hashIndex);if(hashIndex>=0)rest=rest.slice(0,hashIndex);
      const queryIndex=rest.indexOf('?'),search=queryIndex<0?'':rest.slice(queryIndex),pathname=(queryIndex<0?rest:rest.slice(0,queryIndex))||(special&&host?'/':'');
      return legacyFormat({protocol,host,auth,slashes,pathname,search,hash});
    }
    if(!value||typeof value!=='object')typedError('urlObject','one of type object or string',value);
    let protocol=value.protocol||'',auth=value.auth||'',hostname=value.hostname||'',host=false,pathname=value.pathname||'',hash=value.hash||'',search=value.search||'';
    if(auth)auth=encodeURIComponent(auth).replace(/%3A/gi,':')+'@';
    if(value.host)host=auth+value.host;
    else if(hostname){host=auth+(hostname.includes(':')&&!hostname.startsWith('[')?'['+hostname+']':hostname);if(value.port)host+=':'+value.port;}
    if(!search&&value.query&&typeof value.query==='object'){const query=stringQuery(value.query);if(query)search='?'+query;}
    if(protocol&&!protocol.endsWith(':'))protocol+=':';
    const slash=value.slashes||protocol==='file:'||((!protocol||['http:','https:','ftp:','gopher:','file:','ws:','wss:'].includes(protocol))&&host!==false);
    if(slash){host='//'+(host||'');if(pathname&&pathname[0]!=='/')pathname='/'+pathname;}else if(host===false)host='';
    if(hash&&hash[0]!=='#')hash='#'+hash;if(search&&search[0]!=='?')search='?'+search;
    pathname=pathname.replace(/[?#]/g,c=>c==='?'?'%3F':'%23');search=search.replace(/#/g,'%23');
    return protocol+host+pathname+search+hash;
  };
  const url={
    URL:NativeURL,URLSearchParams:NativeURLSearchParams,
    domainToASCII(value){if(arguments.length===0)urlError('ERR_MISSING_ARGS','The "domain" argument must be specified');return domain('domainToASCII',value);},
    domainToUnicode(value){if(arguments.length===0)urlError('ERR_MISSING_ARGS','The "domain" argument must be specified');return domain('domainToUnicode',value);},
    fileURLToPath(value,options){return filePath(value,options,false);},
    fileURLToPathBuffer(value,options){return filePath(value,options,true);},
    pathToFileURL(value,options){
      if(typeof value!=='string')typedError('path','of type string',value);
      const windows=options?.windows??(__skyre_runtime_platform==='windows');
      let resolved=windows&&value.startsWith('\\\\')?value:(windows?win32:posix).resolve(value),host='';
      if(windows&&resolved.startsWith('\\\\')){
        const prefix=resolved.startsWith('\\\\?\\UNC\\')?8:2,index=resolved.indexOf('\\',prefix);
        if(index===-1)urlError('ERR_INVALID_ARG_VALUE',"The argument 'path' Missing UNC resource path. Received "+"'"+resolved.replace(/\\/g,'\\\\')+"'");
        if(index===2)urlError('ERR_INVALID_ARG_VALUE',"The argument 'path' Empty UNC servername. Received "+"'"+resolved.replace(/\\/g,'\\\\')+"'");
        host=domain('domainToASCII',resolved.slice(prefix,index));resolved=resolved.slice(index);
      }
      if((value.endsWith('/')||windows&&value.endsWith('\\'))&&!resolved.endsWith('/')&&!resolved.endsWith('\\'))resolved+='/';
      if(windows)resolved=resolved.replace(/\\/g,'/');
      return parseURL('file://'+host+(resolved.startsWith('/')?'':'/')+encodeFilePath(resolved));
    },
    format(value,options){
      if(!isURL(value))return legacyFormat(value);
      if(options!=null&&(typeof options!=='object'||Array.isArray(options)))typedError('options','of type object',options);
      const formatted=new NativeURL(value.href);options=options??{};
      if(options.auth!=null&&!options.auth){formatted.username='';formatted.password='';}if(options.fragment!=null&&!options.fragment)formatted.hash='';if(options.search!=null&&!options.search)formatted.search='';
      let text=formatted.href;if(options.unicode){const host=formatted.hostname,unicode=domain('domainToUnicode',host);if(host&&unicode!==host){const authority=text.indexOf('//')+2,tail=text.slice(authority),boundary=tail.search(/[/?#]/),end=boundary<0?text.length:authority+boundary,userinfo=text.lastIndexOf('@',end),at=userinfo>=authority?userinfo+1:authority;if(text.startsWith(host,at))text=text.slice(0,at)+unicode+text.slice(at+host.length);}}
      return text;
    },
    urlToHttpOptions(value){
      if(value===null||typeof value!=='object')typedError('url','of type object',value);
      const result=Object.assign(Object.create(null),value,{protocol:value.protocol,hostname:value.hostname?.startsWith('[')?value.hostname.slice(1,-1):value.hostname,hash:value.hash,search:value.search,pathname:value.pathname,path:(value.pathname||'')+(value.search||''),href:value.href});
      if(value.port!=='')result.port=Number(value.port);if(value.username||value.password)result.auth=decodeURIComponent(value.username)+':'+decodeURIComponent(value.password);return result;
    },
  };
  const pathArg=value=>value instanceof URL?{url:value.href}:Buffer.isBuffer(value)?value.toString():pathString(value);
  const options=value=>typeof value==='string'?{encoding:value}:value??{};
  const call=(method,args={})=>{if(args.handle===-1){const error=new Error('file closed');Object.assign(error,{code:'EBADF',syscall:method,errno:-9});throw error;}return unwrap(__skyre_fs(JSON.stringify({method,...args})));};
  const fileData=(value,encoding)=>encode(typeof value==='string'?Buffer.from(value,encoding):view(value));
  const resultData=(value,encoding)=>{const bytes=Buffer.from(decode(value.data));return encoding&&encoding!=='buffer'?bytes.toString(encoding):bytes;};
  const statObject=(value,bigint=false)=>{
    const out={};for(const[key,item]of Object.entries(value)){if(key.startsWith('is'))out[key]=()=>item;else out[key]=bigint&&typeof item==='number'?BigInt(Math.trunc(item)):item;}
    for(const name of ['atime','mtime','ctime','birthtime']){out[name]=new Date(value[name+'Ms']);if(bigint)out[name+'Ns']=BigInt(Math.trunc(value[name+'Ms']*1e6));}return out;
  };
  class FileHandle {
    constructor(handle){this.fd=handle;}
    async close(){if(this.fd>=0){call('close',{handle:this.fd});this.fd=-1;}}
    async readFile(opts){opts=options(opts);return resultData(call('readFile',{handle:this.fd}),opts.encoding);}
    async writeFile(value,opts){opts=options(opts);call('writeFile',{handle:this.fd,data:fileData(value,opts.encoding)});}
    async appendFile(value,opts){opts=options(opts);call('appendFile',{handle:this.fd,data:fileData(value,opts.encoding)});}
    async stat(opts={}){return statObject(call('stat',{handle:this.fd}),opts.bigint);}
    async truncate(length=0){call('truncate',{handle:this.fd,length});}
    async sync(){call('sync',{handle:this.fd});}
    async datasync(){call('datasync',{handle:this.fd});}
    async read(buffer,offset=0,length=buffer?.byteLength,position=null){if(!ArrayBuffer.isView(buffer)){const opts=buffer??{};buffer=opts.buffer??Buffer.alloc(16384);offset=opts.offset??0;length=opts.length??buffer.length-offset;position=opts.position??null;}const bytes=view(buffer);if(offset<0||length<0||offset+length>bytes.length)fail('Read range exceeds buffer','ERR_OUT_OF_RANGE');const result=call('read',{handle:this.fd,length,position});bytes.set(decode(result.data),offset);return {bytesRead:result.bytesRead,buffer};}
    async write(buffer,offset=0,length=buffer?.byteLength,position=null){if(typeof buffer==='string'){position=typeof offset==='number'?offset:null;const encoding=typeof length==='string'?length:'utf8';const result=call('write',{handle:this.fd,data:fileData(buffer,encoding),position});return {bytesWritten:result.bytesWritten,buffer};}const data=view(buffer);if(offset<0||length<0||offset+length>data.length)fail('Write range exceeds buffer','ERR_OUT_OF_RANGE');const result=call('write',{handle:this.fd,data:encode(data.subarray(offset,offset+length)),position});return {bytesWritten:result.bytesWritten,buffer};}
  }
  const fs={
    constants:Object.freeze({F_OK:0,R_OK:4,W_OK:2,X_OK:1,COPYFILE_EXCL:1,COPYFILE_FICLONE:2,COPYFILE_FICLONE_FORCE:4}),
    async readFile(value,opts){opts=options(opts);if(value instanceof FileHandle)return value.readFile(opts);return resultData(call('readFile',{path:pathArg(value),flag:opts.flag}),opts.encoding);},
    async writeFile(value,data,opts){opts=options(opts);if(value instanceof FileHandle)return value.writeFile(data,opts);call('writeFile',{path:pathArg(value),data:fileData(data,opts.encoding),flag:opts.flag,mode:opts.mode,flush:opts.flush});},
    async appendFile(value,data,opts){opts=options(opts);if(value instanceof FileHandle)return value.appendFile(data,opts);call('appendFile',{path:pathArg(value),data:fileData(data,opts.encoding),flag:opts.flag,mode:opts.mode});},
    async open(value,flag='r',mode=0o666){return new FileHandle(call('open',{path:pathArg(value),flag,mode}));},
    async mkdir(value,opts={}){if(typeof opts==='number')opts={mode:opts};const result=call('mkdir',{path:pathArg(value),...opts});return result??undefined;},
    async mkdtemp(value,opts){opts=options(opts);const result=call('mkdtemp',{path:pathArg(value)});return opts.encoding==='buffer'?Buffer.from(result):result;},
    async readdir(value,opts){opts=options(opts);if(opts.recursive)fail('Recursive readdir is unavailable','ERR_NOT_IMPLEMENTED');return call('readdir',{path:pathArg(value),withFileTypes:opts.withFileTypes}).map(item=>opts.withFileTypes?Object.assign(statObject(item),{name:opts.encoding==='buffer'?Buffer.from(item.name):item.name,path:item.parentPath,parentPath:item.parentPath}):opts.encoding==='buffer'?Buffer.from(item):item);},
    async stat(value,opts={}){return statObject(call('stat',{path:pathArg(value)}),opts.bigint);},
    async lstat(value,opts={}){return statObject(call('lstat',{path:pathArg(value)}),opts.bigint);},
    async access(value,mode=0){call('access',{path:pathArg(value),mode});},
    async realpath(value,opts){opts=options(opts);const result=call('realpath',{path:pathArg(value)});return opts.encoding==='buffer'?Buffer.from(result):result;},
    async readlink(value,opts){opts=options(opts);const result=call('readlink',{path:pathArg(value)});return opts.encoding==='buffer'?Buffer.from(result):result;},
    async unlink(value){call('unlink',{path:pathArg(value)});},
    async rm(value,opts={}){call('rm',{path:pathArg(value),...opts});},
    async rmdir(value){call('rmdir',{path:pathArg(value)});},
    async rename(from,to){call('rename',{path:pathArg(from),destination:pathArg(to)});},
    async copyFile(from,to,mode=0){if(mode&4)fail('Forced reflink copy is unavailable','ERR_NOT_IMPLEMENTED');call('copyFile',{path:pathArg(from),destination:pathArg(to),exclusive:!!(mode&1)});},
    async link(from,to){call('link',{path:pathArg(from),destination:pathArg(to)});},
    async symlink(target,value,type){call('symlink',{path:pathArg(target),destination:pathArg(value),type});},
    async truncate(value,length=0){call('truncate',{path:pathArg(value),length});},
    async chmod(value,mode){call('chmod',{path:pathArg(value),mode});},
  };
  Object.defineProperty(globalThis,'__skyreModules',{value:{path,fs,Buffer,url},writable:false,configurable:false});
})();
