// Independent DOM query/action engine. Runs in the selected frame's isolated world.
(args => {
  const registry=globalThis.__skyre_dom_registry??=( {next:0,nonce:globalThis.crypto?.randomUUID?.()??Math.random().toString(36).slice(2),ids:new WeakMap(),nodes:new Map()} );
  const nodeId=e=>{let id=registry.ids.get(e);if(!id){if(registry.nodes.size>=20000)throw new Error('DOM reference limit exceeded');id=String(performance.timeOrigin)+':'+registry.nonce+':'+(++registry.next);registry.ids.set(e,id);registry.nodes.set(id,new WeakRef(e));}return id;};
  const norm = v => String(v ?? '').replace(/\s+/g, ' ').trim();
  const visible = e => { const r=e.getBoundingClientRect(), s=getComputedStyle(e); return r.width>0 && r.height>0 && s.visibility!=='hidden' && s.visibility!=='collapse' && s.display!=='none'; };
  const protectedValue = e => e.type==='password'||['current-password','new-password','one-time-code'].includes(e.autocomplete);
  const attributes=e=>Object.fromEntries(Array.from(e.attributes).filter(a=>!(a.name==='value'&&protectedValue(e))).map(a=>[a.name,a.value]));
  const enabled = e => {if(e.matches(':disabled'))return false;for(let n=e;n;n=n.parentElement??n.getRootNode?.().host){const value=n.getAttribute('aria-disabled')?.toLowerCase();if(value==='true')return false;if(value==='false')return true;}return true;};
  const retarget = e => e.localName==='label'&&e.control?e.control:e;
  const editable = e => !e.readOnly&&e.getAttribute('aria-readonly')!=='true'&&(e.localName==='input'||e.localName==='textarea'||e.isContentEditable);
  const implicit = e => { const t=e.localName;return ({button:'button',textarea:'textbox',select:e.multiple?'listbox':'combobox',option:'option',a:e.hasAttribute('href')?'link':'generic',img:'img',h1:'heading',h2:'heading',h3:'heading',h4:'heading',h5:'heading',h6:'heading',table:'table',tr:'row',td:'cell',th:'columnheader',ul:'list',ol:'list',li:'listitem'})[t] ?? (t==='input'?({checkbox:'checkbox',radio:'radio',range:'slider',number:'spinbutton',button:'button',submit:'button',reset:'button',hidden:'none'})[e.type]??'textbox':'generic'); };
  const name = (e,seen=new Set()) => { if(seen.has(e))return '';seen.add(e);const refs=e.getAttribute('aria-labelledby');if(refs){const v=norm(refs.split(/\s+/).map(id=>{const n=e.ownerDocument.getElementById(id);return n?name(n,seen):''}).join(' '));if(v)return v;}return norm(e.getAttribute('aria-label') ?? (e.labels?.length?Array.from(e.labels).map(l=>l.textContent).join(' '):null) ?? e.getAttribute('alt') ?? ((e.localName==='input'&&['submit','button','reset'].includes(e.type))?e.value:null) ?? e.textContent ?? e.getAttribute('title')); };
  const all = root => {const out=[];for(const e of root.querySelectorAll('*')){out.push(e);if(e.shadowRoot)out.push(...all(e.shadowRoot));if(out.length>20000)throw new Error('DOM node limit exceeded');}return out;};
  const match = (actual, wanted, exact=false) => wanted && typeof wanted==='object' && wanted.regex!==undefined ? new RegExp(wanted.regex,wanted.flags??'').test(actual) : exact?norm(actual)===norm(wanted):norm(actual).toLowerCase().includes(norm(wanted).toLowerCase());
  const dedup = list => [...new Set(list)];
  function query(spec,roots=[document]) {
    if(typeof spec==='string')spec={kind:'css',value:spec};
    if(!spec || typeof spec!=='object')throw new Error('Invalid locator');
    if(spec.kind==='point'){const e=document.elementFromPoint(spec.x,spec.y);return e?[e]:[];}
    if(spec.kind==='xpath'){const out=[];for(const root of roots){const result=document.evaluate(spec.value,root,null,XPathResult.ORDERED_NODE_ITERATOR_TYPE,null);let node;while((node=result.iterateNext()))if(node.nodeType===1)out.push(node);}return dedup(out);}
    if(spec.kind==='nodes')return spec.values.flatMap(value=>query({kind:'node',value},roots)).filter(e=>roots.some(r=>r===document||r.contains(e)));
    if(spec.kind==='node'){const e=registry.nodes.get(String(spec.value))?.deref();return e?.isConnected?[e]:[];}
    if(spec.kind==='chain') { let result=roots;for(const step of spec.steps)result=query(step,result);return result; }
    if(spec.kind==='nth'){const xs=query(spec.base,roots);const i=spec.index<0?xs.length+spec.index:spec.index;return xs[i]?[xs[i]]:[];}
    if(spec.kind==='and'||spec.kind==='or'){const a=query(spec.left,roots),b=query(spec.right,roots);return spec.kind==='and'?a.filter(e=>b.includes(e)):dedup([...a,...b]);}
    if(spec.kind==='filter')return query(spec.base,roots).filter(e=>(spec.hasText===undefined||match(e.textContent,spec.hasText))&&(spec.hasNotText===undefined||!match(e.textContent,spec.hasNotText))&&(spec.has===undefined||query(spec.has,[e]).length>0)&&(spec.hasNot===undefined||query(spec.hasNot,[e]).length===0)&&(spec.visible===undefined||visible(e)===spec.visible));
    const out=[];
    for(const root of roots){
      let candidates;
      if(spec.kind==='css') { candidates=Array.from(root.querySelectorAll(spec.value));for(const el of all(root))if(el.shadowRoot)candidates.push(...el.shadowRoot.querySelectorAll(spec.value)); }
      else candidates=all(root).filter(e=>{switch(spec.kind){
        case 'role': return (e.getAttribute('role')??implicit(e))===spec.value && (spec.name===undefined||match(name(e),spec.name,spec.exact)) && (spec.includeHidden||visible(e));
        case 'text': return match(e.textContent,spec.value,spec.exact) && !Array.from(e.children).some(c=>match(c.textContent,spec.value,spec.exact));
        case 'label': return e.labels && Array.from(e.labels).some(l=>match(l.textContent,spec.value,spec.exact)) || (e.hasAttribute('aria-label')||e.hasAttribute('aria-labelledby'))&&match(name(e),spec.value,spec.exact);
        case 'placeholder': return e.hasAttribute('placeholder')&&match(e.getAttribute('placeholder'),spec.value,spec.exact);
        case 'alt': return e.hasAttribute('alt')&&match(e.getAttribute('alt'),spec.value,spec.exact);
        case 'title': return e.hasAttribute('title')&&match(e.getAttribute('title'),spec.value,spec.exact);
        case 'testid': return e.getAttribute(spec.attribute??'data-testid')===spec.value;
        default: throw new Error('Unsupported locator kind: '+spec.kind);
      }});
      out.push(...candidates);
    }
    return dedup(out);
  }
  if(args.expectedTimeOrigin!==undefined && performance.timeOrigin!==args.expectedTimeOrigin)throw new Error('Document changed before action');
  if(args.expectedUrl!==undefined && location.href!==args.expectedUrl)throw new Error('Document changed before action');
  if(args.operation==='serialize') {
    const limit=6*1024*1024, encoder=new TextEncoder(), strings=[], stringIds=new Map();let bytes=256;
    const charge=value=>{const text=JSON.stringify(value);if(text.length>limit||(bytes+=encoder.encode(text).length+1)>limit)throw new Error('DOM snapshot serialization byte limit exceeded');};
    const intern=value=>{if(typeof value!=='string')return undefined;if(value.length>limit)throw new Error('DOM snapshot serialization byte limit exceeded');let id=stringIds.get(value);if(id!==undefined)return id;charge(value);id=strings.length;strings.push(value);stringIds.set(value,id);return id;};
    const supportedStates=["active","hover","focus","focus-visible","focus-within","enabled","disabled","checked","indeterminate","default","required","optional","valid","invalid","in-range","out-of-range","read-only","read-write","placeholder-shown","autofill","-webkit-autofill","defined","target","target-within","any-link","link","visited","fullscreen","modal","popover-open","open","user-valid","user-invalid","picture-in-picture","playing","paused","seeking","buffering","stalled","muted","volume-locked","dir(ltr)","dir(rtl)"].filter(s=>CSS.supports('selector(:'+s+')'));
    const childIds=root=>{if(root.childNodes.length>20000)throw new Error('DOM snapshot child-node limit exceeded');return Array.from(root.childNodes,nodeId);};
    const result={documentChildNodeIds:childIds(document),title:document.title,url:location.href,contentType:document.contentType,compatMode:document.compatMode,documentElementId:document.documentElement?nodeId(document.documentElement):null,bodyId:document.body?nodeId(document.body):null,supportedStates,selectorFeatures:{langRanges:CSS.supports('selector(:lang("en", "fr"))')},timeOrigin:performance.timeOrigin,selectedIds:args.selector===undefined?null:query(args.selector).map(nodeId)};
    // Keep the CSS element schema separate from ordered Node traversal records.
    // Both kinds share the existing count, depth, string and transport budgets.
    charge(result);const nodes=[],nonElementNodes=[],pending=Array.from(document.childNodes,e=>[e,1]).reverse();
    while(pending.length){const [e,depth]=pending.pop();if(depth>512)throw new Error('DOM snapshot ancestry depth limit exceeded');if(nodes.length+nonElementNodes.length>=20000)throw new Error('DOM node limit exceeded');
      if(e.nodeType!==1){const node={id:nodeId(e),nodeType:e.nodeType,nodeName:e.nodeName,nodeValueString:intern(e.nodeValue),textContentString:intern(e.textContent),dataString:intern(e.data)};charge(node);nonElementNodes.push(node);continue;}
      if(e.attributes.length>4096)throw new Error('DOM snapshot attribute limit exceeded');
      const attributeRecords=Array.from(e.attributes).filter(a=>!(a.name==='value'&&protectedValue(e))).map(a=>{if(a.value.length>limit)throw new Error('DOM snapshot serialization byte limit exceeded');return {name:a.name,localName:a.localName,prefix:a.prefix,namespaceURI:a.namespaceURI,value:a.value};});
      const root=e.getRootNode(),rect=e.getBoundingClientRect();
      const node={id:nodeId(e),childNodeIds:childIds(e),shadowChildNodeIds:e.shadowRoot?childIds(e.shadowRoot):undefined,tree:root===document?'':nodeId(root),shadowHost:root.host?nodeId(root.host):null,shadowRootId:e.shadowRoot?nodeId(e.shadowRoot):null,hasDirectText:Array.from(e.childNodes).some(n=>(n.nodeType===3||n.nodeType===4)&&n.data.length>0),cssStates:supportedStates.filter(s=>e.matches(':'+s)),parent:e.parentElement?nodeId(e.parentElement):null,tag:e.localName,tagName:e.tagName,namespaceURI:e.namespaceURI,objectTag:Object.prototype.toString.call(e).slice(8,-1),attributeRecords,datasetEntries:e.dataset?Object.keys(e.dataset).map(k=>[k,e.dataset[k]]):[],textContentString:intern(e.textContent),innerTextString:intern(e.innerText),value:protectedValue(e)?null:e.value,checked:e.checked,selected:e.selected,disabled:e.disabled,readOnly:e.readOnly,required:e.required,multiple:e.multiple,bounds:{x:rect.x,y:rect.y,width:rect.width,height:rect.height}};
      charge(node);nodes.push(node);
      const children=[...(e.shadowRoot?.childNodes??[]),...e.childNodes];for(let i=children.length-1;i>=0;i--)pending.push([children[i],depth+1]);
    }
    return {...result,nodes,nonElementNodes,strings};
  }
  if(args.operation==='snapshot') {return all(document).filter(visible).slice(0,10000).map(e=>({node_id:nodeId(e),tag:e.localName,role:e.getAttribute('role')??implicit(e),name:name(e),text:norm(e.textContent).slice(0,4000),value:typeof e.value==='string'&&!protectedValue(e)?e.value:undefined}));}
  if(args.operation==='point_info') {
    const stack=document.elementsFromPoint(args.x,args.y), seen=new Set(), out=[];
    for(let el of stack) {while(el&&el!==document.documentElement){if(seen.has(el))break;seen.add(el);const actionable=el.matches('a[href],button,input,textarea,select,[contenteditable],[role],[tabindex]');if(args.includeNonInteractable||actionable){const r=el.getBoundingClientRect(),candidates=[];if(el.id)candidates.push('#'+CSS.escape(el.id));const testId=el.getAttribute('data-testid');if(testId)candidates.push('[data-testid='+JSON.stringify(testId)+']');let css=el.localName;if(el.parentElement)css+=':nth-of-type('+(Array.from(el.parentElement.children).filter(n=>n.localName===el.localName).indexOf(el)+1)+')';candidates.push(css);out.push({ariaName:name(el),boundingBox:{x:r.x,y:r.y,width:r.width,height:r.height},nodeId:null,preview:el.outerHTML.slice(0,400),role:el.getAttribute('role')??implicit(el),selector:{candidates,primary:candidates[0]},tagName:el.localName,testId,visibleText:el.innerText??el.textContent});}el=el.parentElement;}}
    return out;
  }
  const list=query(args.selector);
  switch(args.operation){
    case 'count': return list.length;
    case 'all_text_contents': return list.map(e=>e.textContent);
    case 'read_all': return list.map(root=>{const e=args.relativeSelector?query(args.relativeSelector,[root])[0]:root;return e?{tag:e.localName,attributes:attributes(e),role:e.getAttribute('role')??implicit(e),name:name(e),text:e.textContent,text_content:e.textContent,inner_text:e.innerText,value:protectedValue(e)?null:e.value??null,visible:visible(e),enabled:enabled(e)}:null;});
    case 'inspect': {if(list.length!==1)return {count:list.length,visible:false,enabled:false,editable:false};const e=list[0];return {count:1,nodeIdentity:nodeId(e),formIdentity:e.form?nodeId(e.form):null,visible:visible(e),enabled:enabled(e),editable:!e.readOnly&&(e.localName==='input'||e.localName==='textarea'||e.isContentEditable),tag:e.localName,type:e.type??'',autocomplete:e.autocomplete??'',inputMode:e.inputMode??'',label:name(e),submissionOrigin:e.form?new URL(e.form.action||location.href,location.href).origin:null};}
    case 'state': return {count:list.length,visible:list.length>0&&visible(list[0])};
    case 'is_visible':return list.length>0&&visible(list[0]);
    case 'is_enabled':return list.length>0&&enabled(retarget(list[0]));
  }
  let chosen=list;
  if(list.length>1&&args.expectedNodeIdentity===undefined){const visibleMatches=list.filter(visible);if(visibleMatches.length===1)chosen=visibleMatches;}
  if(chosen.length!==1)throw new Error((list.length>1?'strict mode violation: ':'')+'Strict locator expected one element; found '+list.length);
  const e=['fill','focus','select_option','bound_press'].includes(args.operation)&&args.expectedNodeIdentity===undefined?retarget(chosen[0]):chosen[0];
  const binding=()=>{if(!e.isConnected)throw new Error('Target changed before action');if(args.expectedNodeIdentity!==undefined&&nodeId(e)!==args.expectedNodeIdentity)throw new Error('Target changed before action');if(Object.prototype.hasOwnProperty.call(args,'expectedFormIdentity')&&(e.form?nodeId(e.form):null)!==args.expectedFormIdentity)throw new Error('Target form changed before action');};
  binding();
  if(args.operation==='focus'&&args.sequentialTarget!==undefined){
    let focused=nodeId(e)===args.sequentialTarget,current=e;
    while(focused&&current){const root=current.getRootNode();focused=root.activeElement===current;current=root.host??null;}
    if(!focused)throw new Error('pressSequentially target changed or lost focus while typing');
    return null;
  }
  switch(args.operation){
    case 'text_content':return e.textContent;
    case 'inner_text':return e.innerText;
    case 'get_attribute':return args.name.toLowerCase()==='value'&&protectedValue(e)?null:e.getAttribute(args.name);
    case 'is_enabled':return enabled(e);
    case 'is_visible':return visible(e);
    case 'element_info': {const r=e.getBoundingClientRect();return {tag:e.localName,role:e.getAttribute('role')??implicit(e),name:name(e),text:e.textContent,value:protectedValue(e)?null:e.value??null,attributes:attributes(e),visible:visible(e),enabled:enabled(e),bounds:{x:r.x,y:r.y,width:r.width,height:r.height},checked:e.checked??null};}
  }
  if(args.operation==='set_checked' && e.checked===args.checked)return {unchanged:true};
  if(args.operation==='set_checked' && typeof e.checked!=='boolean')throw new Error('Element is not checkable');
  if(args.operation==='set_checked' && e.type==='radio'&&!args.checked)throw new Error('Cannot uncheck a radio button');
  if(!enabled(e)&&!(args.force&&['point','click','set_checked'].includes(args.operation)))throw new Error('Element is disabled');
  if(args.operation!=='select_option'&&!visible(e)&&!args.force)throw new Error('Element is not visible');
  if(args.operation==='fill') {if(!editable(e))throw new Error('Element is not editable');if(e.localName==='input'&&!['text','search','email','url','tel','password','number','date','time','datetime-local','month','range','week'].includes(e.type))throw new Error('Input type cannot be filled');if(e.type==='number'&&args.value.trim()!==''&&!Number.isFinite(Number(args.value)))throw new Error('Cannot type text into input[type=number]');e.scrollIntoView({block:'center',inline:'nearest'});e.focus();binding();let p=e,set;while((p=Object.getPrototypeOf(p))&&!set)set=Object.getOwnPropertyDescriptor(p,'value')?.set;if(set)set.call(e,args.value);else if(e.isContentEditable)e.textContent=args.value;else throw new Error('Element is not editable');e.dispatchEvent(new Event('input',{bubbles:true}));e.dispatchEvent(new Event('change',{bubbles:true}));return null;}
  if(args.operation==='focus') {
    if(args.requireEditable&&!editable(e))throw new Error('Element is not editable');
    e.scrollIntoView({block:'center',inline:'nearest'});e.focus();binding();return args.captureFocusTarget?nodeId(e):null;
  }
  if(args.operation==='bound_click'){e.click();return null;}
  if(args.operation==='bound_press'){
    if(args.key!=='Enter')throw new Error('Bound authentication key is unsupported');e.focus();binding();
    const key={key:'Enter',code:'Enter',bubbles:true,cancelable:true,composed:true};
    if(e.dispatchEvent(new KeyboardEvent('keydown',key))&&e.dispatchEvent(new KeyboardEvent('keypress',key))){binding();if(e.form){if(['button','input'].includes(e.localName)&&e.type==='submit')e.form.requestSubmit(e);else e.form.requestSubmit();}else if(e.matches('button,a[href]'))e.click();else throw new Error('Bound control has no submit action');}
    e.dispatchEvent(new KeyboardEvent('keyup',key));return null;
  }
  if(args.operation==='select_option') {
    if(e.localName!=='select')throw new Error('Element is not a <select> element');
    let pending=Array.isArray(args.values)?[...args.values]:[args.values];
    const selected=[];
    const matches=(option,wanted,index)=>typeof wanted==='string'?option.value===wanted:
      (wanted.value===undefined||option.value===wanted.value)&&
      (wanted.label===undefined||option.label===wanted.label)&&
      (wanted.index===undefined||index===wanted.index);
    for(const [index,option] of Array.from(e.options).entries()) {
      if(!pending.some(wanted=>matches(option,wanted,index)))continue;
      if(!enabled(option))throw new Error('error:optionnotenabled');
      selected.push(option);
      if(!e.multiple){pending=[];break;}
      pending=pending.filter(wanted=>!matches(option,wanted,index));
    }
    if(pending.length)throw new Error('error:optionsnotfound');
    binding();
    e.value=undefined;
    for(const option of selected)option.selected=true;
    e.dispatchEvent(new Event('input',{bubbles:true,composed:true}));
    e.dispatchEvent(new Event('change',{bubbles:true}));
    return selected.map(option=>option.value);
  }
  if(args.operation==='download_media') {e.scrollIntoView({block:'center'});const m=e.closest('video,audio,img,a')??e.querySelector('video,audio,img,a')??e;const url=m.currentSrc??m.src??m.href;if(!url)throw new Error('Media URL unavailable');const a=document.createElement('a');a.href=url;a.download='';a.rel='noopener';a.hidden=true;document.body.append(a);try{a.click();}finally{a.remove();}return {url};}
  if(!['point','click','set_checked'].includes(args.operation))throw new Error('Unsupported DOM operation: '+args.operation);
  return (async()=>{
    e.scrollIntoView({block:args.scrollAlignment??'center',inline:args.scrollAlignment??'center',behavior:'instant'});
    let r=e.getBoundingClientRect(),stable=0;
    for(let attempt=0;attempt<10;attempt++){
      await new Promise(resolve=>requestAnimationFrame(resolve));
      const next=e.getBoundingClientRect();stable=next.x===r.x&&next.y===r.y&&next.width===r.width&&next.height===r.height?stable+1:0;r=next;
      if(stable>=2)break;
    }
    binding();
    if(!args.force){if(!enabled(e))throw new Error('Element is disabled');if(!visible(e))throw new Error('Element is not visible');}
    if(r.width<=0||r.height<=0)throw new Error('Element does not have a clickable bounding box');
    const x=Math.max(0,r.x+r.width/2),y=Math.max(0,r.y+r.height/2);
    const hit=(e.getRootNode().elementFromPoint?.(x,y)??document.elementFromPoint(x,y));
    if(!args.force && hit!==e&&!e.contains(hit))throw new Error('Element is obscured');
    return {x,y,width:r.width,height:r.height,checked:e.checked??null};
  })();
})
