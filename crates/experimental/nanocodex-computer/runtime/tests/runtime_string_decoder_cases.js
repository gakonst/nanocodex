// Pure incremental decoding cases; no filesystem, provider, or network access.
async function stringDecoderCases() {
  const module = await import('node:string_decoder');
  const {StringDecoder} = module;
  const {Buffer} = await import('node:buffer');
  const rows = [];
  const attempt = fn => {
    try { return {value: fn()}; }
    catch (error) { return {error: {name: error.name, code: error.code, message: error.message}}; }
  };
  const state = decoder => [decoder.lastNeed, decoder.lastTotal, Array.from(decoder.lastChar)];
  const stream = (name, encoding, chunks, finish) => {
    const decoder = new StringDecoder(encoding);
    const result = chunks.map(chunk => [decoder.write(Buffer.from(chunk)), state(decoder)]);
    result.push([decoder.end(finish === undefined ? undefined : Buffer.from(finish)), state(decoder)]);
    result.push([decoder.write(Buffer.from([65])), decoder.end(), state(decoder)]);
    rows.push({name, result});
  };
  rows.push({name:'module-identity', result:[
    Object.keys(module), module === await import('string_decoder'),
    module.default.StringDecoder === StringDecoder,
    StringDecoder.name, StringDecoder.length,
    Object.entries(Object.getOwnPropertyDescriptors(StringDecoder.prototype)).map(([name, d]) =>
      [name, d.enumerable, d.configurable, d.writable ?? null, d.value?.length ?? null]),
  ]});
  for (const encoding of [undefined, null, '', 'UTF8', 'UTF-8', 'utf16le', 'UTF-16LE', 'ucs2', 'UCS-2', 'ascii', 'latin1', 'binary', 'base64', 'base64url', 'hex', 'rot13', 0, false, true]) {
    rows.push({name:'encoding/'+String(encoding), result:attempt(() => {
      const decoder = new StringDecoder(encoding);
      return [decoder.encoding, Object.keys(decoder), state(decoder)];
    })});
  }
  const samples = [
    [], [65], [0], [239,187,191,65], [194,162], [226,130,172], [240,159,167,170],
    [65,226,130,172,90], [237,160,128], [224,128,128], [240,128,128,128],
    [244,144,128,128], [192,175], [245,128,128,128], [255,254,128,191],
    [226,130,65,172], [240,159,65,170], [0,216,0,220], [0,216,65,0],
    [0,216,0], [0,220], [255,254,65,0], [0,216,0,216,0,220],
  ];
  for (const encoding of ['utf8','utf16le','base64','base64url','ascii','latin1','hex']) {
    for (const [index, bytes] of samples.entries()) {
      for (let split=0; split<=bytes.length; split++) {
        stream(encoding+'/'+index+'/split/'+split, encoding, [bytes.slice(0,split),bytes.slice(split)]);
        stream(encoding+'/'+index+'/end/'+split, encoding, [bytes.slice(0,split)], bytes.slice(split));
      }
      stream(encoding+'/'+index+'/bytes', encoding, bytes.map(byte=>[byte]));
    }
    for (let byte=0; byte<256; byte++) stream(encoding+'/single/'+byte,encoding,[[byte]]);
  }
  // Every leading byte with each continuation-boundary class and interruption.
  for (let lead=0; lead<256; lead++) for (const next of [0,65,127,128,143,144,159,160,191,192,224,240,255]) {
    stream('utf8/prefix/'+lead+'/'+next,'utf8',[[lead],[next],[128],[65]]);
  }
  for (const [name, input] of [
    ['string','plain'], ['uint8',new Uint8Array([226,130,172])],
    ['offset-view',new Uint8Array(new Uint8Array([0,226,130,172,0]).buffer,1,3)],
    ['dataview',new DataView(new Uint8Array([0,226,130,172,0]).buffer,1,3)],
    ['uint16',new Uint16Array([0x82e2,0x41ac])], ['arraybuffer',new ArrayBuffer(1)],
    ['array',[]], ['null',null], ['undefined',undefined], ['number',7], ['boolean',true],
  ]) {
    const decoder = new StringDecoder();
    decoder.write(Buffer.from([226]));
    rows.push({name:'input/'+name,result:[attempt(()=>decoder.write(input)),state(decoder),attempt(()=>decoder.end())]});
  }
  for (const encoding of ['utf8','utf16le','base64']) for (const offset of [undefined,0,1,2,3,-1,9,1.5,'1',null]) {
    const decoder = new StringDecoder(encoding);
    decoder.write(Buffer.from([226]));
    rows.push({name:'text/'+encoding+'/'+String(offset),result:[decoder.text(Buffer.from([65,226,130,172]),offset),state(decoder),decoder.end(),state(decoder)]});
  }
  for (const encoding of ['utf8','utf16le','base64']) {
    const decoder = new StringDecoder(encoding);
    decoder.write(Buffer.from([226]));
    const first=decoder.lastChar, second=decoder.lastChar;
    first[0]=65;
    rows.push({name:'mutable-pending/'+encoding,result:[first===second, first.buffer===second.buffer, decoder.end(),state(decoder)]});
    decoder.encoding='hex';
    rows.push({name:'encoding-property/'+encoding,result:[decoder.write(Buffer.from([65,66])),decoder.end(),state(decoder)]});
  }
  // Preserve lone UTF16 surrogates through the Rust JSON boundary by comparing
  // each result's JSON text, rather than asking serde_json to own those strings.
  return rows.map(row => ({name:row.name, result:JSON.stringify(row.result)}));
}
