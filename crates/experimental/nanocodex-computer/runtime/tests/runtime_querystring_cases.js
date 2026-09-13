// Pure query-string behavior. No filesystem, process, provider or network work.
async function querystringCases() {
  const module = await import('node:querystring');
  const qs = module.default;
  const rows = [];
  const attempt = fn => { try { return {value:fn()}; } catch(error) { return {error:{name:error.name,code:error.code,message:error.message}}; } };
  const serialize = value => { const seen=new WeakSet(); return JSON.stringify(value,(_key,item)=>{if(item!==null && typeof item==='object'){if(seen.has(item))return '[shared reference]';seen.add(item);}return item;}); };
  const add = (name,fn) => rows.push({name,result:serialize(attempt(fn))});
  const parsed = (...args) => {const value=qs.parse(...args);return {nullPrototype:Object.getPrototypeOf(value)===null,keys:Object.keys(value),value};};
  add('module',()=>[Object.keys(module),Object.keys(qs),module===undefined, module.parse===qs.parse,qs.decode===qs.parse,qs.encode===qs.stringify,
    Object.entries(qs).map(([key,value])=>[key,value.name,value.length]),Object.keys(Object.getPrototypeOf(qs))]);
  rows.push({name:'module-alias',result:JSON.stringify(module===await import('querystring'))});
  const strings=['','a','a=b','a=b=c','a&b=','a=1&a=2&a=3','__proto__=x&constructor=y&toString=z','2=b&1=a&x=c&01=d','a+b=c+d','%61=%E2%82%AC','%','%0','%GG','%2G','%G2','%00','%ff','%C0%AF','%ED%A0%80','%F4%90%80%80','π=%ff','\ud800=%','a&&b&&','&&=&&','a::b||c::d','a💡b💡💡c','a=b\0c','a=%EF%BB%BFz'];
  for(const [i,text] of strings.entries()) {
    add('parse/'+i,()=>parsed(text));
    add('escape/'+i,()=>qs.escape(text));
    for(const spaces of [undefined,false,true,0,1,'yes']) {
      add('unescape/'+i+'/'+String(spaces),()=>qs.unescape(text,spaces));
      add('unescapeBuffer/'+i+'/'+String(spaces),()=>Array.from(qs.unescapeBuffer(text,spaces)));
    }
  }
  for(let byte=0;byte<256;byte++)for(const prefix of ['', '%E2%82', '%F0%9F%A7']) {
    const text=prefix+'%'+byte.toString(16).padStart(2,'0');
    add('decode-byte/'+prefix+'/'+byte,()=>[qs.unescape(text),Array.from(qs.unescapeBuffer(text,true)),parsed('x='+text)]);
  }
  for(const sep of [undefined,'','&','||','💡','ab',null,0,1,false,[],{}])for(const eq of [undefined,'','=',':','::',null,1]) {
    add('delimiters/'+String(sep)+'/'+String(eq),()=>parsed('a::1||b::2||a::3&=tail',sep,eq));
  }
  const scalars=[undefined,null,false,true,0,1,-1,NaN,Infinity,1n,Symbol('owned'),'s',[],{},new String('x')];
  for(const [i,input] of scalars.entries()) {
    add('parse-input/'+i,()=>parsed(input));
    add('escape-input/'+i,()=>qs.escape(input));
    add('unescape-input/'+i,()=>qs.unescape(input));
    add('unescapeBuffer-input/'+i,()=>Array.from(qs.unescapeBuffer(input)));
    add('stringify-input/'+i,()=>qs.stringify(input));
    add('stringify-value/'+i,()=>qs.stringify({x:input}));
  }
  for(const maxKeys of [undefined,null,false,true,0,-1,1,2,2.1,NaN,Infinity,'2',{},1n]) {
    for(const text of ['a=1&b=2&a=3','&&a=1&&b=2&','a=1&&&','&x=1&']) {
      add('maxKeys/'+String(maxKeys)+'/'+text,()=>parsed(text,'&','=',{maxKeys}));
    }
  }
  add('maxKeys/default-limit',()=>{const value=qs.parse(Array.from({length:1005},(_,i)=>'k'+i+'='+i).join('&'));return[Object.keys(value).length,value.k999,value.k1000];});
  add('array-values',()=>qs.stringify({x:[1,true,null,undefined,{},[],[1,2],NaN,Infinity,1n,Symbol('owned')],empty:[],y:'a b',z:'€'}));
  add('key-order',()=>{const o=Object.create(null);o.z='last';o['2']='second';o['1']='first';o.__proto__='safe';return qs.stringify(o);});
  for(const sep of [undefined,'','&','||',null,0,1,false,[],{}])for(const eq of [undefined,'','=',':',null,1]) {
    add('stringify-delimiters/'+String(sep)+'/'+String(eq),()=>qs.stringify({a:[1,2],b:'x'},sep,eq));
  }
  for(const opts of [undefined,null,0,'x',false,true,{}, {decodeURIComponent:null},{encodeURIComponent:null}]) {
    add('options/'+String(opts),()=>[parsed('a+b=c+d',undefined,undefined,opts),qs.stringify({a:'x y'},undefined,undefined,opts)]);
  }
  for(const [returnedIndex,returned] of ['TEXT',0,false,null,undefined,{},['x']].entries()) {
    add('custom-decode/'+returnedIndex,()=>{const calls=[];const value=qs.parse('a+b=c+d&a+b=e%21&plain=value','&','=',{decodeURIComponent:function(text){calls.push([text,this===undefined]);return returned;}});return[calls,Object.keys(value),value];});
    add('custom-encode/'+returnedIndex,()=>{const calls=[];const value=qs.stringify({a:[1,2],b:'x y'},'&','=',{encodeURIComponent:function(text){calls.push([text,this===undefined]);return returned;}});return[calls,value];});
  }
  add('decode-throws-fallback',()=>{const calls=[];const value=qs.parse('a+b=%E2%82%AC&x=%ff','&','=',{decodeURIComponent:function(text){calls.push(text);throw new Error('owned decoder');}});return[calls,value];});
  add('encode-throws',()=>qs.stringify({x:'a'},'&','=',{encodeURIComponent(){throw Object.assign(new Error('owned encoder'),{code:'OWNED'});}}));
  add('parse-options-access-order',()=>{const calls=[];const options={get maxKeys(){calls.push('maxKeys');return 2},get decodeURIComponent(){calls.push('decoder');return text=>{calls.push(text);return text;}}};const value=qs.parse('a=b&c=d','&','=',options);return[calls,value];});
  add('parse-empty-options-not-read',()=>{const options={get maxKeys(){throw new Error('maxKeys read')},get decodeURIComponent(){throw new Error('decoder read')}};return parsed('',undefined,undefined,options);});
  add('stringify-getters-order',()=>{const calls=[];const o={get a(){calls.push('a');return [1,2]},get b(){calls.push('b');return 3}};const value=qs.stringify(o,'&','=',{get encodeURIComponent(){calls.push('encoder');return text=>{calls.push(text);return text;}}});return[calls,value];});
  add('coercion-escape',()=>{const calls=[];const value=qs.escape({[Symbol.toPrimitive](hint){calls.push(hint);return 'a b'}});return[calls,value];});
  add('coercion-delimiters',()=>{const calls=[];const sep={toString(){calls.push('separator');return '|'}},eq={toString(){calls.push('equals');return ':'}};const value=qs.stringify({a:[1,2],b:3},sep,eq);return[calls,value];});
  add('mutated-hooks',()=>{const oldEscape=qs.escape,oldUnescape=qs.unescape;const calls=[];try{qs.escape=text=>{calls.push(['escape',text]);return '<'+text+'>'};qs.unescape=text=>{calls.push(['unescape',text]);return '['+text+']'};return[qs.stringify({a:'x y'}),qs.parse('a=b+c'),calls,module.escape===oldEscape,module.unescape===oldUnescape];}finally{qs.escape=oldEscape;qs.unescape=oldUnescape;}});
  for(const sep of ['aa','aab','aba','aaa','&&','💡a']) for(const eq of ['=','aa','aba','::']) for(const text of ['aaab=x','aaba=1aaba=2','ababa=3','a=1&&&b=2','aabaaaab','💡a💡ax=y']) {
    add('overlap/'+sep+'/'+eq+'/'+text,()=>parsed(text,sep,eq));
  }
  add('empty-components-callback',()=>{const calls=[];const value=qs.parse('a=&=b&=&&a','&','=',{decodeURIComponent(text){calls.push(text);return text}});return[calls,value];});
  add('empty-array-stringify',()=>qs.stringify({a:[],b:[1],c:[],d:[2],e:[]}));
  add('key-and-value-getter-order',()=>{const calls=[];const value=qs.stringify({get key(){calls.push('get-value');return [1,2]}},'&','=',{encodeURIComponent(text){calls.push('encode:'+text);return text}});return[calls,value];});
  for(const method of ['parse','stringify'])for(const argument of ['separator','equals']) {
    add('symbol-delimiter/'+method+'/'+argument,()=>qs[method](method==='parse'?'a=b':{a:'b'},argument==='separator'?Symbol('owned'):'&',argument==='equals'?Symbol('owned'):'='));
  }
  add('decode-pop-object',()=>{const value={pop:true,push(input){this.called=input===this}};return qs.parse('a=b&a=c','&','=',{decodeURIComponent(text){return text==='a'?'key':value}});});
  add('custom-decode-symbol',()=>{let i=0;return qs.parse('a=b&a=c','&','=',{decodeURIComponent(){return i++%2?1:Symbol.for('owned-querystring')}})});
  add('custom-encode-symbol',()=>qs.stringify({a:1},'&','=',{encodeURIComponent(){return Symbol('owned')}}));
  for(const lead of [0xd7ff,0xd800,0xdbff,0xdc00,0xdfff,0xe000]) for(const suffix of ['', 'A', '\0', '\ud800','\udc00']) {
    const text=String.fromCharCode(lead)+suffix;
    add('surrogate-escape/'+lead+'/'+JSON.stringify(suffix),()=>qs.escape(text));
    add('surrogate-stringify/'+lead+'/'+JSON.stringify(suffix),()=>qs.stringify({x:text}));
  }
  return rows;
}
