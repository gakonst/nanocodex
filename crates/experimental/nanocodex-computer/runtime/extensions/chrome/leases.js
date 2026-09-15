// Independent provider state. Only the extension service worker imports this.
// Session storage survives worker suspension; local storage identifies this installation.
(() => {
  const KEY='skyre.provider.leases.v1', INSTANCE='skyre.provider.instance.v1';
  const copy=value=>structuredClone(value);
  const validId=value=>typeof value==='string'&&value.length>0&&value.length<=256;
  const validTab=value=>Number.isSafeInteger(value)&&value>=0;
  class LeaseStore {
    constructor(chrome){this.chrome=chrome;this.leases=new Map();this.sessions=new Map();this.queue=Promise.resolve();this.initialized=this.load();}
    async load(){
      const local=await this.chrome.storage.local.get(INSTANCE);
      this.instanceId=validId(local[INSTANCE])?local[INSTANCE]:crypto.randomUUID();
      if(local[INSTANCE]!==this.instanceId)await this.chrome.storage.local.set({[INSTANCE]:this.instanceId});
      const saved=(await this.chrome.storage.session.get(KEY))[KEY];
      if(saved===undefined)return;
      if(saved?.version!==1||!Array.isArray(saved.leases)||!Array.isArray(saved.sessions))throw new Error('Invalid persisted Skyre provider state');
      for(const [id,session] of saved.sessions){
        if(!validId(id)||!session||!(session.turnId===null||validId(session.turnId))||!Array.isArray(session.retired)||session.retired.some(id=>!validId(id)))throw new Error('Invalid persisted browser session');
        this.sessions.set(id,session);
      }
      for(const [id,lease]of saved.leases){
        if(!validTab(id)||lease?.tabId!==id||!validId(lease.sessionId)||!validId(lease.turnId)||!validId(lease.instanceId)||!['agent','user'].includes(lease.origin)||!['active','handoff'].includes(lease.state)||!Number.isFinite(lease.claimedAt)||!this.sessions.has(lease.sessionId)||lease.mark!==undefined&&!['deliverable','handoff'].includes(lease.mark))throw new Error('Invalid persisted tab lease');
        // An installation identity change invalidates control, never adopts it.
        if(lease.instanceId!==this.instanceId)throw new Error('Tab leases belong to another extension instance');
        this.leases.set(id,lease);
      }
    }
    async transaction(change){
      const leases=new Map([...this.leases].map(([key,value])=>[key,copy(value)]));
      const sessions=new Map([...this.sessions].map(([key,value])=>[key,copy(value)]));
      const result=change({leases,sessions});
      await this.chrome.storage.session.set({[KEY]:{version:1,leases:[...leases],sessions:[...sessions]}});
      this.leases=leases;this.sessions=sessions;return result;
    }
    serial(action){const result=this.queue.then(async()=>{await this.initialized;return action()});this.queue=result.catch(()=>{});return result;}
    context(value){if(!value||!validId(value.sessionId)||!validId(value.turnId))throw new Error('Browser session and turn context are required');return value;}
    current(context){const c=this.context(context),session=this.sessions.get(c.sessionId);if(!session||session.turnId!==c.turnId)throw new Error('Browser session turn is not active');return session;}
    async begin(context){
      const c=this.context(context),old=this.sessions.get(c.sessionId);
      if(old?.turnId===c.turnId)return;
      if(old?.retired.includes(c.turnId))throw new Error('Browser turn is stale');
      if(this.sessions.size>=1024&&!old)throw new Error('Browser session limit exceeded');
      if((old?.retired.length??0)>=10000)throw new Error('Browser turn history limit exceeded');
      const existing=new Set((await this.chrome.tabs.query({})).map(tab=>tab.id));
      await this.transaction(({leases,sessions})=>{
        const session=sessions.get(c.sessionId)??{turnId:null,retired:[],activeTabId:null,name:'Nanocodex'};
        if(session.turnId!==null)session.retired.push(session.turnId);
        session.turnId=c.turnId;
        for(const[id,lease]of leases){if(lease.sessionId!==c.sessionId)continue;if(!existing.has(id)){leases.delete(id);continue;}
          if(lease.state==='handoff'&&lease.isActiveHandoff)session.activeTabId=id;
          lease.state='active';lease.turnId=c.turnId;delete lease.mark;delete lease.isActiveHandoff;
        }
        sessions.set(c.sessionId,session);
      });
    }
    require(context,id){this.current(context);const lease=this.leases.get(id);if(!lease||lease.state!=='active'||lease.sessionId!==context.sessionId||lease.turnId!==context.turnId||lease.instanceId!==this.instanceId)throw new Error('Tab is not owned by the active browser session');return lease;}
    async claim(context,id,origin){
      this.current(context);if(!validTab(id))throw new Error('Invalid tab ID');
      const previous=this.leases.get(id);
      if(previous?.state==='active'&&previous.sessionId!==context.sessionId)throw new Error('Tab is already owned by another browser session');
      if(this.leases.size>=10000&&!previous)throw new Error('Tab lease limit exceeded');
      await this.transaction(({leases,sessions})=>{
        const old=leases.get(id);
        leases.set(id,old?.state==='active'?{...old,turnId:context.turnId}:{tabId:id,sessionId:context.sessionId,turnId:context.turnId,origin,claimedAt:Date.now(),instanceId:this.instanceId,state:'active'});
        sessions.get(context.sessionId).activeTabId=id;
      });
    }
    async mark(context,id,mark){this.require(context,id);if(!['deliverable','handoff'].includes(mark))throw new Error('Invalid tab mark');await this.transaction(({leases})=>{leases.get(id).mark=mark});}
    async activate(context,id){this.require(context,id);await this.transaction(({sessions})=>{sessions.get(context.sessionId).activeTabId=id});}
    async list(context){this.current(context);const all=await this.chrome.tabs.query({}),existing=new Set(all.map(tab=>tab.id));
      if([...this.leases.keys()].some(id=>!existing.has(id)))await this.transaction(({leases})=>{for(const id of leases.keys())if(!existing.has(id))leases.delete(id)});
      const owned=all.filter(tab=>{const l=this.leases.get(tab.id);return l?.sessionId===context.sessionId&&l.state==='active'&&l.turnId===context.turnId});
      const session=this.sessions.get(context.sessionId);const selected=owned.some(t=>t.id===session.activeTabId)?session.activeTabId:(owned.find(t=>t.active)?.id??owned[0]?.id??null);
      return owned.map(tab=>({...tab,active:tab.id===selected}));
    }
    async remove(id){await this.transaction(({leases,sessions})=>{leases.delete(id);for(const session of sessions.values())if(session.activeTabId===id)session.activeTabId=null});}
    async replace(added,removed){await this.transaction(({leases,sessions})=>{const lease=leases.get(removed);if(!lease)return;if(leases.has(added))throw new Error('Replacement tab is already leased');leases.delete(removed);leases.set(added,{...lease,tabId:added});for(const session of sessions.values())if(session.activeTabId===removed)session.activeTabId=added});}
    snapshotLease(id){const lease=this.leases.get(id);return lease?copy(lease):null;}
    async adopt(snapshot,id){
      if(!snapshot||snapshot.state!=='active'||this.sessions.get(snapshot.sessionId)?.turnId!==snapshot.turnId)return false;
      const current=this.leases.get(snapshot.tabId);
      if(!current||['tabId','sessionId','turnId','origin','claimedAt','instanceId','state','isActiveHandoff'].some(key=>current[key]!==snapshot[key])||this.leases.has(id))return false;
      await this.transaction(({leases})=>{leases.set(id,{tabId:id,sessionId:current.sessionId,turnId:current.turnId,origin:'agent',claimedAt:Date.now(),instanceId:this.instanceId,state:'active',...(Object.hasOwn(current,'viewportSize')?{viewportSize:current.viewportSize}:{})})});return true;
    }
    async viewport(context,id,value){this.current(context);if(id!==null)this.require(context,id);await this.transaction(({leases,sessions})=>{if(id===null)sessions.get(context.sessionId).pendingViewport={turnId:context.turnId,value};else{leases.get(id).viewportSize=value;delete sessions.get(context.sessionId).pendingViewport}});}
    async takeViewport(context,id){const lease=this.require(context,id);if(Object.hasOwn(lease,'viewportSize'))return lease.viewportSize;const pending=this.current(context).pendingViewport;if(pending?.turnId!==context.turnId)return undefined;await this.transaction(({leases,sessions})=>{leases.get(id).viewportSize=pending.value;delete sessions.get(context.sessionId).pendingViewport});return pending.value;}
    async finish(context,liveIds){
      this.current(context);const session=this.current(context);
      await this.transaction(({leases,sessions})=>{
        for(const[id,lease]of leases){if(lease.sessionId!==context.sessionId||lease.turnId!==context.turnId||lease.state!=='active')continue;
          if(lease.mark==='handoff'&&liveIds.has(id)){delete lease.mark;lease.state='handoff';if(session.activeTabId===id)lease.isActiveHandoff=true;}
          else leases.delete(id);
        }
        const state=sessions.get(context.sessionId);state.retired.push(context.turnId);state.turnId=null;state.activeTabId=null;delete state.pendingViewport;
      });
    }
  }
  globalThis.SkyreLeaseStore=LeaseStore;
})();
