// Apply the owned virtual clipboard to the focused editable control.
(items => {
 const entries=items.flatMap(item=>item.entries),data=new DataTransfer();
 const textOf=entry=>entry.text??new TextDecoder().decode(Uint8Array.from(atob(entry.base64),c=>c.charCodeAt(0)));
 for(const entry of entries)if(entry.mimeType.startsWith('text/'))data.setData(entry.mimeType,textOf(entry));
 if(!document.hasFocus())throw new Error('Focused document changed before paste');
 let doc=document,element=doc.activeElement;
 for(let depth=0;depth<32;depth++){
   if(element?.shadowRoot?.activeElement){element=element.shadowRoot.activeElement;continue;}
   if(element?.localName==='iframe'){if(!element.contentDocument)throw new Error('Virtual paste into a cross-origin focused frame requires a selected frame');doc=element.contentDocument;element=doc.activeElement;continue;}break;
 }
 if(!element||element.disabled||element.readOnly||!(element.isContentEditable||['input','textarea'].includes(element.localName)))throw new Error('Focused element is not editable');
 const event=new ClipboardEvent('paste',{bubbles:true,cancelable:true,composed:true,clipboardData:data});
 if(!element.dispatchEvent(event))return null;
 if(!element.dispatchEvent(new InputEvent('beforeinput',{bubbles:true,cancelable:true,composed:true,inputType:'insertFromPaste',data:data.getData('text/plain'),dataTransfer:data})))return null;
 if(element.isContentEditable){const html=data.getData('text/html');if(!doc.execCommand(html?'insertHTML':'insertText',false,html||data.getData('text/plain')))throw new Error('Editable content rejected clipboard insertion');}
 else {if(!['text','search','email','url','tel','password','number'].includes(element.type)&&element.localName!=='textarea')throw new Error('Control does not support text paste');let text=data.getData('text/plain');const start=element.selectionStart??element.value.length,end=element.selectionEnd??start;if(element.maxLength>=0)text=text.slice(0,Math.max(0,element.maxLength-(element.value.length-(end-start))));const value=element.value.slice(0,start)+text+element.value.slice(end);let p=element,set;while((p=Object.getPrototypeOf(p))&&!set)set=Object.getOwnPropertyDescriptor(p,'value')?.set;if(!set)throw new Error('Input value setter unavailable');set.call(element,value);try{element.setSelectionRange(start+text.length,start+text.length);}catch{}element.dispatchEvent(new InputEvent('input',{bubbles:true,composed:true,inputType:'insertFromPaste',data:text}));}
 return null;
})
