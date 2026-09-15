// Independent AX value action. Native setters preserve control semantics; event
// handlers may reject a change, so completion always checks the actual control.
function(value) {
  const element=this, role=element.getAttribute('role');
  const validate=(name,readonly=false)=>{
    if(!element.isConnected)throw new Error(name+' is no longer connected');
    if(element.matches(':disabled')||element.getAttribute('aria-disabled')==='true')throw new Error(name+' is disabled');
    if(readonly&&(element instanceof HTMLInputElement||element instanceof HTMLTextAreaElement)&&element.readOnly)throw new Error(name+' is read-only');
  };
  const nativeSetter=(prototype,key,message)=>{
    const setter=Object.getOwnPropertyDescriptor(prototype,key)?.set;
    if(!setter)throw new Error(message);
    return next=>setter.call(element,next);
  };
  const changed=()=>{
    element.dispatchEvent(new Event('input',{bubbles:true,composed:true}));
    element.dispatchEvent(new Event('change',{bubbles:true}));
  };
  if(element instanceof HTMLInputElement&&['checkbox','radio'].includes(element.type)) {
    const name=element.type==='radio'?'Radio button':role==='switch'?'Switch':'Checkbox';
    if(!['0','1','false','true'].includes(value))throw new Error(name+' value must be 0, 1, false, or true');
    validate(name);
    const checked=value==='1'||value==='true';
    const needsEvent=element.checked!==checked||(element.type==='checkbox'&&element.indeterminate);
    const setChecked=nativeSetter(HTMLInputElement.prototype,'checked',name+' native setter is unavailable');
    element.focus();setChecked(checked);
    if(element.type==='checkbox')nativeSetter(HTMLInputElement.prototype,'indeterminate',name+' indeterminate setter is unavailable')(false);
    if(needsEvent)changed();
    if(!element.isConnected||element.checked!==checked||(element.type==='checkbox'&&element.indeterminate))throw new Error(name+' did not retain the requested state');
    return 'done';
  }
  if(element instanceof HTMLSelectElement) {
    validate('Select');
    if(element.multiple)throw new Error('Multi-select controls are not supported');
    const options=Array.from(element.options);
    let option=options.find(candidate=>candidate.value===value);
    if(!option) {
      const matches=options.filter(candidate=>candidate.label===value||candidate.text===value);
      if(matches.length!==1)throw new Error(matches.length?'Select option label '+JSON.stringify(value)+' is ambiguous':'Select option '+JSON.stringify(value)+' was not found');
      option=matches[0];
    }
    if(option.disabled||(option.parentElement instanceof HTMLOptGroupElement&&option.parentElement.disabled))throw new Error('Select option '+JSON.stringify(value)+' is disabled');
    const set=nativeSetter(HTMLSelectElement.prototype,'value','Select native value setter is unavailable');
    const before=element.value,needsEvent=before!==option.value||!option.selected;
    element.focus();set(option.value);
    if(element.value!==option.value||!option.selected){set(before);throw new Error('Select rejected option '+JSON.stringify(value));}
    if(needsEvent)changed();
    if(!element.isConnected||element.value!==option.value||!option.selected)throw new Error('Select did not retain the requested option');
    return 'done';
  }
  if(element instanceof HTMLInputElement||element instanceof HTMLTextAreaElement) {
    const textarea=element instanceof HTMLTextAreaElement,name=textarea?'Text area':'Input';
    if(!textarea&&element.type==='file')throw new Error('File inputs cannot be set programmatically');
    validate(name,true);element.focus();
    let requested=value;
    if(!textarea&&element.type==='color') {
      if(!/^#[0-9a-f]{6}$/i.test(value))throw new Error('Color value must use #rrggbb');
      requested=value.toLowerCase();
    }
    const set=nativeSetter(textarea?HTMLTextAreaElement.prototype:HTMLInputElement.prototype,'value',name+' native value setter is unavailable');
    const before=element.value;set(requested);
    if(element.value!==requested){set(before);throw new Error(name+' rejected value '+JSON.stringify(value));}
    if(before!==requested)changed();
    if(!element.isConnected||element.value!==requested)throw new Error(name+' did not retain the requested value');
    return 'done';
  }
  if(role==='tab') {
    if(value!=='1'&&value!=='true')throw new Error('Tab value must be 1 or true');
    validate('Tab');return element.getAttribute('aria-selected')==='true'?'done':'needs-click';
  }
  if(['checkbox','radio','switch','combobox','listbox','slider','spinbutton'].includes(role)||element.hasAttribute('aria-pressed'))throw new Error('Custom ARIA controls do not expose a standard value setter');
  if(element instanceof HTMLElement&&element.isContentEditable) {
    validate('Editable element');element.focus();
    const before=element.textContent??'';element.textContent=value;
    if(before!==value){element.dispatchEvent(new InputEvent('input',{bubbles:true,composed:true,data:value,inputType:'insertText'}));element.dispatchEvent(new Event('change',{bubbles:true}));}
    if(!element.isConnected||element.textContent!==value)throw new Error('Editable element did not retain the requested value');
    return 'done';
  }
  throw new Error('Accessibility element does not expose a standard value setter');
}
