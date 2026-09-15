// Evaluated in a fresh QuickJS runtime with data only, no host/network/page handles.
(async (snapshot,fn,arg,selector,all) => {
 const leafRecords=snapshot.nonElementNodes??[];
 if(snapshot.nodes.length+leafRecords.length>20000)throw new Error('DOM node limit exceeded');
 const index=new Map(snapshot.nodes.map(n=>[n.id,{...n,children:[]}]))
 const domString=v=>`${v}`;
 const required=(args,iface,method)=>{if(!args.length)throw new TypeError(`Failed to execute '${method}' on '${iface}': 1 argument required, but only 0 present.`);return args[0];};
 const stringArgument=(args,iface,method)=>{const value=required(args,iface,method);if(typeof value==='symbol')throw new TypeError(`Failed to execute '${method}' on '${iface}': Cannot convert a Symbol value to a string`);return value;};
 const itemIndex=(args,iface)=>{let value=required(args,iface,'item');if(value!==null&&(typeof value==='object'||typeof value==='function')){const original=value,exotic=original[Symbol.toPrimitive];if(exotic!=null){value=Reflect.apply(exotic,original,['number']);if(value!==null&&(typeof value==='object'||typeof value==='function'))throw new TypeError('Cannot convert object to primitive value');}else{let found=false;for(const key of ['valueOf','toString']){const method=original[key];if(typeof method==='function'){const next=Reflect.apply(method,original,[]);if(next===null||(typeof next!=='object'&&typeof next!=='function')){value=next;found=true;break;}}}if(!found)throw new TypeError('Cannot convert object to primitive value');}}if(typeof value==='bigint'||typeof value==='symbol')throw new TypeError(`Failed to execute 'item' on '${iface}': Cannot convert a ${typeof value==='bigint'?'BigInt':'Symbol'} value to a number`);return Number(value)>>>0;};
 const rootOptions=options=>{if(options==null)return false;if(typeof options!=='object'&&typeof options!=='function')throw new TypeError("Failed to execute 'getRootNode' on 'Node': The provided value is not of type 'GetRootNodeOptions'.");return Boolean(options.composed);};
 const namespaceArguments=(args,iface,method)=>{if(args.length<2)throw new TypeError(`Failed to execute '${method}' on '${iface}': 2 arguments required, but only ${args.length} present.`);const namespace=args[0]==null?null:domString(stringArgument([args[0]],iface,method));return [namespace||null,domString(stringArgument([args[1]],iface,method))];};
 const asciiLower=s=>domString(s).replace(/[A-Z]/g,c=>c.toLowerCase());
 const tokens=s=>[...new Set(String(s).split(/[\t\n\f\r ]+/).filter(Boolean))];
 const selectCaptured=globalThis.__skyre_snapshot_query;delete globalThis.__skyre_snapshot_query;
 const references=new WeakMap([...index].map(([id,n])=>[n,id]));
 const metadata=new WeakMap(snapshot.nodes.map(raw=>[index.get(raw.id),raw]));
 const callbackRequired=fn=>{if(typeof fn==='function')return;const type=typeof fn;let label=type;if(fn===null)label='object null';else if(type==='string')label+=' '+JSON.stringify(fn);else if(type==='number'||type==='boolean')label+=' '+String(fn);throw new TypeError(label+' is not a function');};
 const nodeList=values=>{const list={};values.forEach((v,i)=>Object.defineProperty(list,i,{value:v,enumerable:true}));Object.defineProperties(list,{length:{value:values.length},item:{value:(...args)=>values[itemIndex(args,'NodeList')]??null},entries:{value:()=>values.entries()},keys:{value:()=>values.keys()},values:{value:()=>values.values()},forEach:{value:(fn,thisArg)=>{callbackRequired(fn);values.forEach((v,i)=>Reflect.apply(fn,thisArg,[v,i,list]));}},[Symbol.iterator]:{value:()=>values.values()},[Symbol.toStringTag]:{value:'NodeList'}});return list;};
 const htmlCollection=values=>{const list={};const named=(...args)=>{const name=domString(stringArgument(args,'HTMLCollection','namedItem'));return name?values.find(n=>n.id===name||n.namespaceURI==='http://www.w3.org/1999/xhtml'&&n.getAttribute('name')===name)??null:null;};Object.defineProperties(list,{length:{value:values.length},item:{value:(...args)=>values[itemIndex(args,'HTMLCollection')]??null},namedItem:{value:named},[Symbol.iterator]:{value:()=>values.values()},[Symbol.toStringTag]:{value:'HTMLCollection'}});values.forEach((n,i)=>Object.defineProperty(list,i,{value:n,enumerable:true}));for(const n of values)for(const name of [n.id,n.namespaceURI==='http://www.w3.org/1999/xhtml'?n.getAttribute('name'):null])if(name&&!(name in list))Object.defineProperty(list,name,{value:n});return list;};
 const exceptionData=new WeakMap();
 class DOMException extends Error {constructor(message='',name='Error'){const actualMessage=domString(message);super(actualMessage);delete this.message;const actualName=domString(name);exceptionData.set(this,{message:actualMessage,name:actualName});}get name(){return exceptionData.get(this)?.name??'Error';}get message(){return exceptionData.get(this)?.message??'';}get code(){return this.name==='SyntaxError'?12:0;}}
 Object.defineProperty(DOMException.prototype,Symbol.toStringTag,{value:'DOMException'});globalThis.DOMException=DOMException;
 const select=(root,args,operation,method,iface)=>{
  const css=domString(stringArgument(args,iface,method));const result=JSON.parse(selectCaptured(css,references.get(root)??'',operation));
  if(result.error){if(result.error==='syntax'){throw new DOMException(`Failed to execute '${method}' on '${iface}': ${css.length===0?'The provided selector is empty.':`'${css}' is not a valid selector.`}`,'SyntaxError');}throw new Error('Read-only snapshot selector exceeded its query budget');}
  return result.ids.map(id=>index.get(id));
 };
 const queries=(root,iface)=>{root.querySelectorAll=(...args)=>nodeList(select(root,args,'query','querySelectorAll',iface));root.querySelector=(...args)=>select(root,args,'first','querySelector',iface)[0]??null;};
 for(const n of index.values()){
  if(n.textContentString!==undefined)n.textContent=snapshot.strings[n.textContentString];if(n.innerTextString!==undefined)n.innerText=snapshot.strings[n.innerTextString];
  const namespaces=new Map((n.attributeNamespaces??[]).map(a=>[a.name,a]));
  const records=n.attributeRecords??(n.attributeOrder??Object.keys(n.attributes)).filter(name=>Object.hasOwn(n.attributes,name)).map(name=>({name,value:n.attributes[name],localName:name,prefix:null,namespaceURI:null,...namespaces.get(name)}));
  const attrNames=records.map(a=>a.name),attrs=Object.create(null),plain=Object.create(null);
  for(const a of records){if(!Object.hasOwn(attrs,a.name))attrs[a.name]=a.value;if(a.namespaceURI==null&&!Object.hasOwn(plain,a.localName))plain[a.localName]=a.value;}
  n.parentElement=index.get(n.parent)??null;n.parentElement?.children.push(n);if(!Object.hasOwn(n,'namespaceURI'))n.namespaceURI='http://www.w3.org/1999/xhtml';
  const html=n.namespaceURI==='http://www.w3.org/1999/xhtml'&&(snapshot.contentType??'text/html')==='text/html';
  const attributeName=html?asciiLower:domString;
  n.tagName=n.tagName??(html?n.tag.toUpperCase():n.tag);n.localName=n.tag;
  n.id=plain.id??'';n.className=n.namespaceURI==='http://www.w3.org/2000/svg'?{baseVal:plain.class??'',animVal:plain.class??''}:plain.class??'';n.nodeType=1;n.nodeName=n.tagName;n.nodeValue=null;
  n.getAttribute=(...args)=>attrs[attributeName(stringArgument(args,'Element','getAttribute'))]??null;n.hasAttribute=(...args)=>Object.hasOwn(attrs,attributeName(stringArgument(args,'Element','hasAttribute')));
  n.getAttributeNames=()=>[...attrNames];
  const attrNodes=records.map(a=>({...a,nodeName:a.name,nodeValue:a.value,textContent:a.value,nodeType:2,ownerElement:n,specified:true}));
  const byNamespace=(args,iface,method)=>{const [ns,local]=namespaceArguments(args,iface,method);return attrNodes.find(a=>(a.namespaceURI??null)===ns&&a.localName===local)??null;};
  n.getAttributeNS=(...args)=>byNamespace(args,'Element','getAttributeNS')?.value??null;n.hasAttributeNS=(...args)=>byNamespace(args,'Element','hasAttributeNS')!==null;n.getAttributeNodeNS=(...args)=>byNamespace(args,'Element','getAttributeNodeNS');
  for(const attr of attrNodes){for(const key of Object.keys(attr))Object.defineProperty(attr,key,{enumerable:false});Object.defineProperty(attr,Symbol.toStringTag,{value:'Attr'});}
  const namedMap={getNamedItemNS:(...args)=>byNamespace(args,'NamedNodeMap','getNamedItemNS'),item:(...args)=>attrNodes[itemIndex(args,'NamedNodeMap')]??null,getNamedItem:(...args)=>{const name=attributeName(stringArgument(args,'NamedNodeMap','getNamedItem'));return attrNodes.find(a=>a.name===name)??null;},[Symbol.iterator]:()=>attrNodes[Symbol.iterator]()};
  for(const key of Reflect.ownKeys(namedMap))Object.defineProperty(namedMap,key,{enumerable:false});
  Object.defineProperty(namedMap,'length',{value:attrNodes.length});
  Object.defineProperty(namedMap,Symbol.toStringTag,{value:'NamedNodeMap'});
  attrNodes.forEach((a,i)=>{Object.defineProperty(namedMap,String(i),{value:a,enumerable:true});if(!(a.name in namedMap))Object.defineProperty(namedMap,a.name,{value:a});});
  n.attributes=namedMap;n.getAttributeNode=(...args)=>namedMap.getNamedItem(stringArgument(args,'Element','getAttributeNode'));
  const data={},dataKeys=[];Object.defineProperty(data,Symbol.toStringTag,{value:'DOMStringMap'});
  const datasetEntries=n.datasetEntries??records.filter(a=>a.name.startsWith('data-')&&!/[A-Z]/.test(a.name.slice(5))).map(a=>[a.name.slice(5).replace(/-([a-z])/g,(_,c)=>c.toUpperCase()),a.value]);
  for(const [key,value] of datasetEntries){if(!dataKeys.includes(key))dataKeys.push(key);Object.defineProperty(data,key,{value,writable:true,enumerable:true,configurable:true});}
  n.dataset=new Proxy(data,{ownKeys:()=>[...dataKeys.filter(k=>Object.hasOwn(data,k)),...Reflect.ownKeys(data).filter(k=>!dataKeys.includes(k))]});
  const classes=tokens(plain.class??'');const classList={item:(...args)=>classes[itemIndex(args,'DOMTokenList')]??null,contains:(...args)=>classes.includes(domString(stringArgument(args,'DOMTokenList','contains'))),entries:()=>classes.entries(),keys:()=>classes.keys(),values:()=>classes.values(),[Symbol.iterator]:()=>classes.values(),toString:()=>plain.class??'',value:plain.class??''};
  classList.forEach=(fn,thisArg)=>{callbackRequired(fn);classes.forEach((v,i)=>Reflect.apply(fn,thisArg,[v,i,classList]));};
  for(const key of Reflect.ownKeys(classList))Object.defineProperty(classList,key,{enumerable:false});
  Object.defineProperty(classList,'length',{value:classes.length});Object.defineProperty(classList,Symbol.toStringTag,{value:'DOMTokenList'});
  classes.forEach((v,i)=>Object.defineProperty(classList,String(i),{value:v,enumerable:true}));n.classList=classList;
  n.getBoundingClientRect=()=>{const {x=0,y=0,width=0,height=0}=metadata.get(n).bounds;return {x,y,width,height,top:Math.min(y,y+height),left:Math.min(x,x+width),right:Math.max(x,x+width),bottom:Math.max(y,y+height),toJSON(){return {x,y,width,height,top:this.top,left:this.left,right:this.right,bottom:this.bottom}}};};n.matches=(...args)=>select(n,args,'matches','matches','Element').length!==0;
  queries(n,'Element');n.closest=(...args)=>select(n,args,'closest','closest','Element')[0]??null;
 }
 const nodes=[...index.values()];
 const documentNodes=nodes.filter(n=>!n.tree);
 const document={title:snapshot.title,URL:snapshot.url,contentType:snapshot.contentType??'text/html',compatMode:snapshot.compatMode??'CSS1Compat',nodeType:9,nodeName:'#document',parentNode:null,parentElement:null,body:Object.hasOwn(snapshot,'bodyId')?(index.get(snapshot.bodyId)??null):documentNodes.find(n=>(n.tag==='body'||n.tag==='frameset')&&n.namespaceURI==='http://www.w3.org/1999/xhtml')??null,documentElement:index.get(snapshot.documentElementId)??documentNodes.find(n=>!n.parentElement)??null,getElementById:(...args)=>{const id=domString(stringArgument(args,'Document','getElementById'));return id?documentNodes.find(n=>n.id===id)??null:null}};
 references.set(document,'');queries(document,'Document');
 const shadows=new Map();
 for(const n of nodes){const id=n.shadowRootId??n.tree;if(id&&!shadows.has(id)){const host=n.shadowRootId?n:index.get(n.shadowHost);const root={nodeType:11,nodeName:'#document-fragment',host,mode:'open',parentNode:null,parentElement:null,ownerDocument:document,children:[]};references.set(root,id);queries(root,'DocumentFragment');root.getElementById=(...args)=>{const value=domString(stringArgument(args,'DocumentFragment','getElementById'));return value?nodes.find(n=>metadata.get(n).tree===id&&n.id===value)??null:null};shadows.set(id,root);}}
 document.children=nodes.filter(n=>!n.parentElement&&!n.tree);
 for(const n of nodes){n.ownerDocument=document;n.shadowRoot=shadows.get(n.shadowRootId)??null;n.parentNode=n.parentElement??shadows.get(n.tree)??document;if(!n.parentElement&&n.tree)n.parentNode.children.push(n);n.getRootNode=options=>{const composed=rootOptions(options);let root=shadows.get(metadata.get(n).tree)??document;while(composed&&root.host)root=shadows.get(metadata.get(root.host).tree)??document;return root;};}
 for(const root of [document,...nodes,...shadows.values()]){root.childElementCount=root.children.length;root.firstElementChild=root.children[0]??null;root.lastElementChild=root.children.at(-1)??null;root.children.forEach((n,i)=>{n.previousElementSibling=root.children[i-1]??null;n.nextElementSibling=root.children[i+1]??null;});}
 for(const root of shadows.values())root.getRootNode=options=>rootOptions(options)?root.host.getRootNode({composed:true}):root;
 document.getRootNode=options=>{rootOptions(options);return document;};
 // Older element-only snapshots remain readable; current captures provide the
 // complete ordered children, including leaves before/after the document root.
 const leaves=leafRecords.map(raw=>{
  if(typeof raw.id!=='string'||index.has(raw.id)||![3,4,7,8,10].includes(raw.nodeType))throw new Error('Invalid DOM snapshot non-element node');
  const value=key=>raw[key]===undefined?null:snapshot.strings[raw[key]];
  const n={nodeType:raw.nodeType,nodeName:raw.nodeName,nodeValue:value('nodeValueString'),textContent:value('textContentString'),ownerDocument:document,parentNode:null,parentElement:null,childNodes:nodeList([]),firstChild:null,lastChild:null};
  if(raw.dataString!==undefined){n.data=value('dataString');n.length=n.data.length;}
  n.getRootNode=options=>{const composed=rootOptions(options);return n.parentNode?.getRootNode({composed})??n;};
  index.set(raw.id,n);return n;
 });
 const assigned=new Set();
 for(const root of [document,...nodes,...shadows.values()]){
  const raw=root===document?null:root.host?metadata.get(root.host):metadata.get(root);
  const ids=root===document?snapshot.documentChildNodeIds:root.host?raw.shadowChildNodeIds:raw.childNodeIds;
  if(ids!==undefined&&(!Array.isArray(ids)||ids.length>20000))throw new Error('Invalid DOM snapshot child-node list');
  const children=ids===undefined?[...root.children]:ids.map(id=>{const n=index.get(id);if(!n)throw new Error('Invalid DOM snapshot child-node reference');return n;});
  for(const [i,n] of children.entries()){
   if(assigned.has(n)||(n.nodeType===1&&n.parentNode!==root))throw new Error('Invalid DOM snapshot child-node ownership');
   assigned.add(n);n.parentNode=root;n.parentElement=root.nodeType===1?root:null;
   n.previousSibling=children[i-1]??null;n.nextSibling=children[i+1]??null;
  }
  root.childNodes=nodeList(children);root.firstChild=children[0]??null;root.lastChild=children.at(-1)??null;
 }
 if([...nodes,...leaves].some(n=>!assigned.has(n)))throw new Error('Invalid DOM snapshot missing child-node reference');
 document.previousSibling=null;document.nextSibling=null;document.nodeValue=null;
 for(const root of shadows.values()){root.previousSibling=null;root.nextSibling=null;root.nodeValue=null;}
 for(const n of leaves){for(const key of Object.keys(n))Object.defineProperty(n,key,{enumerable:false});Object.defineProperty(n,Symbol.toStringTag,{value:({3:'Text',4:'CDATASection',7:'ProcessingInstruction',8:'Comment',10:'DocumentType'})[n.nodeType]});}
 const elementProperties=new Set(['childNodes','firstChild','lastChild','previousSibling','nextSibling','nodeValue','children','parentElement','namespaceURI','tagName','localName','id','className','nodeType','nodeName','getAttribute','hasAttribute','getAttributeNames','getAttributeNS','hasAttributeNS','getAttributeNodeNS','attributes','getAttributeNode','dataset','classList','getBoundingClientRect','matches','querySelectorAll','querySelector','closest','ownerDocument','shadowRoot','parentNode','getRootNode','previousElementSibling','nextElementSibling','childElementCount','firstElementChild','lastElementChild','textContent','innerText','value','checked','selected','disabled','readOnly','required','multiple']);
 for(const root of [document,...nodes,...shadows.values()])root.children=htmlCollection(root.children);
 for(const n of nodes){for(const key of Object.keys(n)){if(elementProperties.has(key))Object.defineProperty(n,key,{enumerable:false});else delete n[key];}Object.defineProperty(n,Symbol.toStringTag,{value:metadata.get(n).objectTag??(n.namespaceURI==='http://www.w3.org/1999/xhtml'?'HTMLElement':'Element')});}
 for(const root of shadows.values()){for(const key of Object.keys(root))Object.defineProperty(root,key,{enumerable:false});Object.defineProperty(root,Symbol.toStringTag,{value:'ShadowRoot'});}
 const location={href:snapshot.url};globalThis.document=document;globalThis.location=location;globalThis.window={document,location};
 const query=(s,roots=[document])=>{if(typeof s==='string')s={kind:'css',value:s};switch(s.kind){
  case 'chain':{let v=roots;for(const x of s.steps)v=query(x,v);return v;}
  case 'nth':{const v=query(s.base,roots);return [v[s.index<0?v.length+s.index:s.index]].filter(Boolean);}
  case 'and':{const right=query(s.right,roots);return query(s.left,roots).filter(e=>right.includes(e));}
  case 'or':return [...new Set([...query(s.left,roots),...query(s.right,roots)])];
  case 'css':return [...new Set(roots.flatMap(r=>select(r,[s.value],'query','querySelectorAll',r===document?'Document':'Element')))];
  default:throw new Error('Read-only locator evaluation currently requires CSS selectors');
 }};
 let result;if(selector===null)result=fn(arg);else{const elements=snapshot.selectedIds?snapshot.selectedIds.map(id=>index.get(id)).filter(Boolean):query(selector);if(!all&&elements.length!==1)throw new Error('Strict locator expected one element');result=fn(all?elements:elements[0],arg);}
 return await result;
})
