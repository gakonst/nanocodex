// Resolve the live AX target before sending browser keyboard input. Returning
// true is the only successful outcome; never redirect input to another element.
async function(requireEditable) {
  const element=this, started=performance.now();
  const validate=()=>{
    if(!element.isConnected)throw new Error('Browser input target is detached');
    const style=getComputedStyle(element);
    if(!element.getClientRects().length || style.visibility==='hidden' || style.visibility==='collapse' || element.closest('[inert]'))throw new Error('Browser input target is not visible');
    if(element.matches(':disabled') || element.getAttribute('aria-disabled')==='true')throw new Error('Browser input target is disabled');
    if(requireEditable) {
      const textInput=element instanceof HTMLInputElement && ['text','search','email','url','tel','password','number'].includes(element.type);
      if(!(element.isContentEditable || element instanceof HTMLTextAreaElement || textInput) || element.readOnly || element.getAttribute('aria-readonly')==='true')throw new Error('Browser input target is not editable');
    }
  };
  validate();
  element.focus();
  do {
    validate();
    if(element.getRootNode().activeElement===element)return true;
    if(performance.now()-started>=250)break;
    await new Promise(resolve=>setTimeout(resolve,10));
  } while(true);
  throw new Error('Browser input target could not be focused within 250 ms');
}
