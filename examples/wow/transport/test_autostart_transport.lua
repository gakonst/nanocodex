-- Full addon lifecycle with public WoW APIs mocked; no real UI or input.
local textureCount,frames,state
local function boot(db,options)
    options=options or {}
    frames={} textureCount=0
    state={combat=false,focus=false,ctrl=false,shift=false,alt=false,meta=false,bindings={},time=123.456}
    NanocodexWowDB=db
    UIParent={}
    InCombatLockdown=function() return state.combat end
    GetCurrentKeyBoardFocus=function() return state.focus end
    IsControlKeyDown=function() return state.ctrl end
    IsAltKeyDown=function() return state.alt end
    IsShiftKeyDown=function() return state.shift end
    IsMetaKeyDown=function() return state.meta end
    IsModifierKeyDown=function() return state.ctrl or state.alt or state.shift or state.meta end
    GetBindingAction=function(key,overrides) assert(overrides==true) return state.bindings[key] or '' end
    GetTime=function() return state.time end
    GetServerTime=function() return 1790000000 end
    local forbidden=function() error('must not change bindings, focus, game actions or graphics') end
    SetBinding=forbidden SetOverrideBinding=forbidden RunMacroText=forbidden SetCVar=forbidden
    local methods={SetFocus=forbidden}
    for _,name in ipairs({'SetSize','SetPoint','SetFrameStrata','EnableMouse'}) do methods[name]=function() end end
    methods.Hide=function(self) self.hidden=true end
    methods.SetScript=function(self,name,fn) self[name]=fn end
    methods.RegisterEvent=function(self,event) self.events[event]=true end
    methods.UnregisterEvent=function(self,event) self.events[event]=nil end
    methods.EnableKeyboard=function(self,value)
        assert(not state.combat,'keyboard changes in combat')
        if value then assert(not state.focus and self.propagate==true) end
        self.keyboard=value
    end
    methods.SetPropagateKeyboardInput=function(self,value)
        assert(not state.combat and value==true,'input must always propagate') self.propagate=value
    end
    methods.CreateTexture=function()
        textureCount=textureCount+1
        return {SetSize=function() end,SetPoint=function() end,SetColorTexture=function() end}
    end
    CreateFrame=function()
        local frame=setmetatable({events={}},{__index=methods}) frames[#frames+1]=frame
        if options.missingFrameAPI then frame[options.missingFrameAPI]=false end
        return frame
    end
    local NS={}
    assert(loadfile('addon/Transport.lua'))('Nanocodex',NS)
    local T=NS.Transport
    local function event(event,name) T.lifecycle:OnEvent(event,name) end
    local function tick(dt)
        T.lifecycle:OnUpdate(dt or .25)
        if T.frame then T.frame:OnUpdate() end
    end
    local function loaded()
        event('ADDON_LOADED','Nanocodex') event('PLAYER_ENTERING_WORLD')
    end
    return NS,T,event,tick,loaded
end

local NS,T,event,tick,loaded=boot(nil)
assert(#frames==1 and textureCount==0 and not T.frame and not T.lifecycle.keyboard)
assert(NS.TransportStatus().guard_reason=='waiting for ADDON_LOADED')
event('ADDON_LOADED','AnotherAddon') tick()
assert(not T.loaded and NanocodexWowDB==nil)
event('ADDON_LOADED','Nanocodex') tick()
assert(not T.frame and NS.TransportStatus().guard_reason=='waiting for PLAYER_ENTERING_WORLD')
state.combat=true event('PLAYER_ENTERING_WORLD')
assert(not T.frame and textureCount==0 and NS.TransportStatus().guard_reason=='combat lockdown')
state.combat=false state.focus={} tick()
assert(not T.frame and NS.TransportStatus().guard_reason=='keyboard focus is held by an edit box')
state.focus=false state.bindings['CTRL-SHIFT-F10']='AN_ACTION' tick()
assert(not T.frame and NS.TransportStatus().guard_reason=='binding conflict: CTRL-SHIFT-F10')
state.bindings={} state.alt=true tick()
assert(not T.frame and NS.TransportStatus().guard_reason=='Alt modifier is held')
state.alt=false state.ctrl=true tick()
assert(not T.frame and NS.TransportStatus().guard_reason:find('partial chord',1,true))
state.ctrl=false state.meta=true tick()
assert(not T.frame and NS.TransportStatus().guard_reason=='Meta modifier is held')
state.meta=false tick(.1) assert(not T.frame) tick(.15)
assert(T.frame and T.link.mode=='chord' and T.app and T.link.ready)
assert(textureCount==1024 and #frames==2 and T.frame.keyboard and T.frame.propagate)
local session=T.link.session
assert(session>=1 and session<=4294967295 and session==math.floor(session))
assert(NanocodexWowDB.bridgeLastSession==session and NanocodexWowDB.bridgeAutoStart==true)
local decoded=assert(T.Decode(T.link:Packet()))
assert(decoded.session==session and decoded.seq==0 and decoded.ack==0 and decoded.ready)
local status=NS.TransportStatus()
assert(not status.connected and status.mode=='chord' and status.auto_start and status.ready)
assert(status.message:find('No recent peer transport frame',1,true))
assert(T.DebugStatus():find('session '..session,1,true))
-- Ordinary and unmodified function keys propagate without entering the carrier.
for _,key in ipairs({'W','SPACE','F1','F10','F11','F21','LCTRL'}) do
    T.frame:OnKeyDown(key) assert(T.frame.propagate and T.keys.buffer==nil)
end
-- Polling, zoning, and missing peers never rotate a session or replay pending work.
assert(select(2,NS.TransportSend('request'))=='pending')
local pending=T.link.pending
state.time=999999 tick() loaded() tick()
assert(T.link.session==session and T.link.pending==pending and T.link.tx==1 and #frames==2)
event('PLAYER_LEAVING_WORLD')
assert(not T.link.ready and NS.TransportStatus().guard_reason=='waiting for PLAYER_ENTERING_WORLD')
tick() assert(T.link.session==session)
event('PLAYER_ENTERING_WORLD') tick()
assert(T.link.ready and T.link.pending==pending and T.link.session==session)
-- Dynamic guards reset incomplete frames, report the reason, and resume the same link.
state.ctrl=true state.shift=true T.frame:OnKeyDown('F10') T.frame:OnKeyDown('F1')
assert(T.keys.buffer~=nil)
state.focus={} tick()
assert(not T.link.ready and T.keys.buffer==nil and NS.TransportStatus().guard_reason=='keyboard focus is held by an edit box')
state.focus=false state.ctrl=false state.shift=false tick() assert(T.link.ready)
-- Slash off persists even during combat; no protected frame mutation until safe.
SlashCmdList={}
C_Timer={After=function(_,fn) fn() end}
assert(loadfile('addon/Nanocodex/Core.lua'))('Nanocodex',NS)
local notices={}
NS.Notify=function(kind,message) notices[#notices+1]={kind,message} end
state.combat=true
local old=T.frame
SlashCmdList.NANOCODEXWOW('bridge off')
assert(NanocodexWowDB.bridgeAutoStart==false and T.stopRequested and not T.link.ready)
assert(old.keyboard and not old.hidden and notices[1][2]:find('deferred until combat ends',1,true))
assert(NS.TransportStatus().state=='stopping' and not NS.TransportSend('other'))
state.ctrl=true state.shift=true old:OnKeyDown('F10') assert(T.keys.buffer==nil)
tick() assert(old.keyboard and not old.hidden)
state.combat=false tick()
assert(not T.frame and not old.keyboard and old.hidden and not old.OnKeyDown and not old.OnUpdate)
assert(NS.TransportStatus().state=='disabled' and not NS.TransportStatus().auto_start)
loaded() tick() assert(not T.frame)
-- Simulate reload with SavedVariables: off remains off, even when all guards clear.
local db=NanocodexWowDB
NS,T,event,tick,loaded=boot(db)
loaded() tick() assert(not T.frame and textureCount==0 and db.bridgeAutoStart==false)
assert(T.DebugStatus()=='Bridge disabled; automatic startup is off')
-- Existing explicit enable is still an escape hatch that clears the persisted opt-out.
assert(T.Enable(4321,nil,'chord')) assert(db.bridgeAutoStart and db.bridgeLastSession==4321)
assert(not T.Enable(4322,nil,'chord') and T.link.session==4321)
NS,T,event,tick,loaded=boot(db)
loaded() assert(T.link.session==4322 and T.link.mode=='chord')
-- Wrap stays nonzero, and corrupt/uninitialized saved values cannot create a bad packet.
for _,previous in ipairs({4294967295,0,-1,1.5,'invalid',math.huge,0/0}) do
    NS,T,event,tick,loaded=boot({bridgeLastSession=previous})
    loaded() assert(T.Decode(T.link:Packet()).session>=1)
    if previous==4294967295 then assert(T.link.session==1) end
end
-- Off before startup must not be undone by delayed events/polling, nor allocate a session.
NS,T,event,tick,loaded=boot({bridgeLastSession=1234})
event('ADDON_LOADED','Nanocodex')
assert(T.Disable()) loaded() tick()
assert(not T.frame and NanocodexWowDB.bridgeLastSession==1234 and not NanocodexWowDB.bridgeAutoStart)
-- Missing APIs remain fail-closed with exact diagnostics, including the Meta hypothesis.
for _,name in ipairs({'InCombatLockdown','GetCurrentKeyBoardFocus','IsControlKeyDown','IsAltKeyDown',
    'IsShiftKeyDown','IsMetaKeyDown','GetBindingAction','GetTime','CreateFrame'}) do
    NS,T,event,tick,loaded=boot({bridgeLastSession=99})
    local fn=_G[name] _G[name]=nil
    loaded() tick()
    assert(not T.frame and textureCount==0 and NanocodexWowDB.bridgeLastSession==99)
    local reason='missing API: '..name
    assert(NS.TransportStatus().guard_reason==reason and T.DebugStatus():find(reason,1,true),name)
    local _,err=T.Enable(43,nil,'chord') assert(err==reason)
    _G[name]=fn tick() assert(T.frame and T.link.session==100)
end
-- Unsupported frame API is diagnosed once; retries must not leak frames/textures.
for _,name in ipairs({'EnableKeyboard','SetPropagateKeyboardInput'}) do
    NS,T,event,tick,loaded=boot({}, {missingFrameAPI=name})
    loaded() local count=#frames
    for _=1,20 do tick() end
    assert(not T.frame and textureCount==0 and #frames==count)
    assert(NS.TransportStatus().guard_reason=='missing frame API: '..name)
end
-- Invalid explicit sessions do not throw or create a keyboard receiver.
NS,T,event,tick,loaded=boot({bridgeAutoStart=false}) loaded()
for _,session in ipairs({0,-1,4294967296,1.1,'invalid',math.huge,0/0}) do
    local link,err=T.Enable(session,nil,'chord')
    assert(not link and err=='session must be a nonzero 32-bit integer')
end
assert(#frames==1 and textureCount==0)
print('Lua automatic chord startup: all assertions passed')

-- Re-enable through the actual slash path, including delayed chat/combat guards.
NS,T,event,tick,loaded=boot({bridgeAutoStart=false,bridgeLastSession=400})
loaded()
SlashCmdList={}
assert(loadfile('addon/Nanocodex/Core.lua'))('Nanocodex',NS)
NS.Notify=function() end
state.focus={}
SlashCmdList.NANOCODEXWOW('bridge on')
assert(NanocodexWowDB.bridgeAutoStart and not T.frame)
state.focus=false state.combat=true tick() assert(not T.frame)
state.combat=false tick() assert(T.link.session==401)
assert(select(2,NS.TransportSend('pending'))=='pending')
local retained=T.link.pending
SlashCmdList.NANOCODEXWOW('bridge auto') tick()
assert(T.link.session==401 and T.link.pending==retained)
state.combat=true T.Disable()
assert(not T.StartAutomatic() and not NanocodexWowDB.bridgeAutoStart)
state.combat=false tick()
assert(T.StartAutomatic() and T.link.session==402)
print('PASS: explicit automatic opt-in waits for guards and preserves active identities')
