// Inert native window adapters; images are synthetic bytes, no desktop IO.
(async () => {
  const results=[];
  for(const platform of ['linux','windows']) {
    const calls=[],values=[],errors=[];
    let shots=1,accessibility=true,captureId=0;
    const record=(name,args)=>calls.push([name,args]);
    const computer={target:platform,
      list_apps:async()=>[{id:'fixture',name:'Fixture',windows:[{id:7}]},{id:'stopped',name:'Stopped',windows:[]}],
      list_windows:async()=>[{id:7,title:'Fixture'}],get_window:async args=>{record('get_window',args);return {id:args.id,title:'Fixture'};},
      get_window_state:async args=>{record('get_window_state',args);return {window:{id:7,title:'Refreshed '+(++captureId)},ax_tree_source:'fixture',ax_tree:{to_string:()=> 'Fixture tree'},accessibility:accessibility?{tree:'Fixture tree',focused_element:4,selected_text:'selected',selected_elements:['one','two'],document_text:'document'}:null,screenshots:Array.from({length:shots},(_,i)=>({id:100+i,bytes:new Uint8Array([1,2]),url:'data:image/png;base64,AQI='}))};}};
    for(const name of ['click','drag','scroll','set_value','press_key','type_text','perform_secondary_action'])computer[name]=async args=>{record(name,args);};
    const api=await __testCreateCUA({computer,readDocumentation:async()=>'',getNodeRepl:()=>undefined});
    const keys=Object.keys(api).sort(),apps=await api.listApps({emit:false}),windows=await api.listWindows({emit:false});
    async function fail(id,fn){try{await fn();errors.push([id,'NO ERROR']);}catch(error){errors.push([id,error.message]);}}
    for(const reference of ['Fixture',{windowId:0},{windowId:1.5},{windowId:Number.MAX_SAFE_INTEGER+1},null])await fail('bad-reference',()=>api.getApp(reference));
    if(platform==='linux')await fail('missing-window',()=>api.getApp({windowId:9}));
    const app=await api.getApp({windowId:7});
    values.push(await app.getAXState({emit:false}));
    values.push([...await app.getScreenshot()]);
    await app.click([10,20],{mouseButton:'right',clickCount:2});
    await app.click(3);await app.drag([1,2],[3,4]);
    for(const direction of ['up','d','left','r'])await app.scroll([5,6],direction,{pixels:12});
    if(platform==='linux')await app.scroll(3,'down');
    await fail('bad-scroll-distance',()=>app.scroll([1,2],'up',3));
    await fail('zero-pixels',()=>app.scroll([1,2],'up',{pixels:0}));
    await fail('infinite-pixels',()=>app.scroll([1,2],'up',{pixels:Infinity}));
    if(platform==='windows') {
      await fail('scroll-needs-point',()=>app.scroll(3,'down',{pixels:12}));
      await fail('scroll-direction',()=>app.scroll([1,2],'diagonal',{pixels:12}));
      await fail('screenshot-emit-false',()=>app.getScreenshot({emit:false}));
      await fail('both-emit-false',()=>app.getAXStateAndScreenshot({emit:false}));
    }
    await fail('selectText',()=>app.selectText(1,'text'));
    if(platform==='linux')await fail('setValue',()=>app.setValue(1,'value'));else await app.setValue(1,'value');
    await app.paste('paste');await app.paste('plain',{format:'text'});
    await fail('html-paste',()=>app.paste('html',{format:'html'}));
    await app.pressKey('Ctrl+A');await app.typeText('typed');await app.performSecondaryAction(2,'expand');
    const both=await app.getAXStateAndScreenshot();values.push({state:both.state,screenshot:[...both.screenshot]});
    accessibility=false;values.push(await app.getAXState({emit:false}));await app.click([10,20]);
    shots=0;values.push(await app.getAXStateAndScreenshot());await fail('no-screenshot',()=>app.getScreenshot());
    shots=2;await fail('multi-screenshot',()=>app.getScreenshot());await fail('multi-both',()=>app.getAXStateAndScreenshot());
    results.push({platform,keys,apps,windows,values,errors,calls});
  }
  return results;
})()
