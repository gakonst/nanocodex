// Inert inventories: no browser connection, claims only record fixture calls.
(async () => {
  const calls=[],results=[];
  let controlled=[{id:'owned',title:'Owned',url:'https://fixture.test/owned'}];
  let user=[{id:'owned',providerTabId:'native-owned',title:'Owned',url:'https://fixture.test/owned'},{id:'user',providerTabId:'native-user',title:'User',url:'https://fixture.test/user'}];
  let infos=[{id:'chrome-one',type:'extension',metadata:{extensionInstanceId:'instance-one'}},{id:'iab',type:'iab'},{id:'other',type:'cdp'}];
  let claim=true;
  const tab=id=>({id,async getAXState(options){calls.push(['state',id,options]);return 'state:'+id;}});
  const browsers={async list(){calls.push(['list']);return infos;},async get(id){calls.push(['get',id]);return {browserId:id,documentation:async()=>'',tabs:{list:async()=>controlled,get:async id=>{calls.push(['tab',id]);return tab(id);}},user:{openTabs:async()=>{calls.push(['user']);return user;},...(claim?{claimTab:async value=>{calls.push(['claim',value]);return tab(value.id);}}:{})}};}};
  const api=await __testCreateCUA({browsers,readDocumentation:async()=>'',getNodeRepl:()=>undefined});
  async function check(id,fn){calls.length=0;try{const value=await fn();results.push({id,value:value?.id??value?.browserId??value,calls:calls.slice()});}catch(error){results.push({id,error:error.message,calls:calls.slice()});}}
  const mention=(changes={},name='chrome')=>'plugin://'+name+'@openai-bundled/?'+new URLSearchParams({mention:'tab-v1',browserId:'instance-one',tabId:'native-owned',title:'Owned',url:'https://fixture.test/owned',...changes});
  await check('extension',()=>api.getBrowser({extensionInstanceId:'instance-one'}));
  await check('extension-conflict',()=>api.getBrowser({id:'chrome-one',extensionInstanceId:'instance-one'}));
  await check('extension-missing',()=>api.getBrowser({extensionInstanceId:'missing'}));
  infos.push({...infos[0],id:'duplicate'});
  await check('extension-ambiguous',()=>api.getBrowser({extensionInstanceId:'instance-one'}));infos.pop();
  await check('owned-id',()=>api.getTab('owned',{browser:'chrome-one'}));
  await check('provider-id-enriched',()=>api.getTab('native-owned',{browser:'chrome-one'}));
  await check('user-claim',()=>api.getTab('native-user',{browser:'chrome-one'}));
  claim=false;await check('unclaimable',()=>api.getTab('native-user',{browser:'chrome-one'}));claim=true;
  await check('missing',()=>api.getTab('missing',{browser:'chrome-one'}));
  await check('empty',()=>api.getTab(''));
  await check('url',()=>api.getTab({url:'https://fixture.test/owned'},{browser:'chrome-one'}));
  await check('url-needs-browser',()=>api.getTab({url:'https://fixture.test/owned'}));
  await check('url-relative',()=>api.getTab({url:'/owned'},{browser:'chrome-one'}));
  user.push({id:'duplicate',url:'https://fixture.test/owned'});
  await check('url-ambiguous',()=>api.getTab({url:'https://fixture.test/owned'},{browser:'chrome-one'}));user.pop();
  await check('mention',()=>api.getTab({mention:mention()}));
  await check('mention-browser-mismatch',()=>api.getTab({mention:mention()},{browser:'other'}));
  await check('mention-stale',()=>api.getTab({mention:mention({title:'Stale'})}));
  await check('mention-missing-profile',()=>api.getTab({mention:mention({browserId:'missing'})}));
  await check('mention-invalid-url',()=>api.getTab({mention:'https://fixture.test/'}));
  await check('mention-invalid-fields',()=>api.getTab({mention:mention({mention:'tab-v2'})}));
  await check('mention-duplicate-field',()=>api.getTab({mention:mention()+'&title=Owned'}));
  await check('iab-mention',()=>api.getTab({mention:mention({browserId:'ignored'},'browser')}));
  infos.push({...infos[0],id:'duplicate'});
  await check('mention-ambiguous-browser',()=>api.getTab({mention:mention()}));infos.pop();
  controlled.push({id:'duplicate',providerTabId:'native-owned',title:'Owned',url:'https://fixture.test/owned'});
  controlled[0].providerTabId='native-owned';
  await check('ambiguous-provider-id',()=>api.getTab('native-owned',{browser:'chrome-one'}));
  return results;
})()
