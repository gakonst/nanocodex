// Compare the factory's decorator against the fallback using an inert AX adapter.
(async () => {
  const calls=[],outcomes=[];
  await __testCreateCUA({browsers:{},readDocumentation:async()=>'',getNodeRepl:()=>undefined});
  const methods={};
  for(const name of ['paste','pressKey','typeText'])methods[name]=async(...args)=>{calls.push([name,...args]);};
  const tab=__testDecorateTab({id:'fixture',ax:methods});
  async function invoke(name,args) {
    calls.length=0;
    try {await tab[name](...args);outcomes.push({name,args,calls:calls.slice()});}
    catch(error){outcomes.push({name,args,error:error.message,calls:calls.slice()});}
  }
  for(const name of ['paste','pressKey','typeText']) {
    for(const index of [null,0,42,-1,1.5,'text',undefined]) await invoke(name,[index,name==='pressKey'?'Return':'text']);
  }
  await invoke('paste',[3,'<b>text</b>',{format:'html'}]);
  return outcomes;
})()
