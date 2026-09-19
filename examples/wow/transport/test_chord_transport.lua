local NS={}
local nums={1,2,3,5,6,7,8,9,10,11}
assert(loadfile('addon/Transport.lua'))('Nanocodex',NS)
local T=NS.Transport
local ctrl,alt,shift,meta,focus,combat=false,false,false,false,false,false
local bindings,checked={},{}
InCombatLockdown=function() return combat end
GetCurrentKeyBoardFocus=function() return focus end
IsControlKeyDown=function() return ctrl end
IsAltKeyDown=function() return alt end
IsShiftKeyDown=function() return shift end
IsMetaKeyDown=function() return meta end
IsModifierKeyDown=function() return ctrl or alt or shift or meta end
GetBindingAction=function(key,overrides) assert(overrides==true) checked[key]=true return bindings[key] or '' end
GetTime=function() return 0 end
UIParent={}
local forbidden=function() error('must not change bindings, focus or protected actions') end
SetBinding=forbidden SetOverrideBinding=forbidden RunMacroText=forbidden
local methods={SetFocus=forbidden}
for _,name in ipairs({'SetSize','SetPoint','SetFrameStrata','EnableMouse','Hide'}) do methods[name]=function() end end
methods.SetScript=function(self,name,fn) self[name]=fn end
methods.EnableKeyboard=function(self,value) self.keyboard=value end
methods.SetPropagateKeyboardInput=function(self,value) assert(value==true) self.propagate=value end
methods.CreateTexture=function() return {SetSize=function() end,SetPoint=function() end,SetColorTexture=function() end} end
CreateFrame=function() return setmetatable({},{__index=methods}) end
local deliveries=0
local function deliver(value) assert(value=='hello') deliveries=deliveries+1 end
assert(not T.Enable(42,deliver,'unknown'))
-- Every chord binding, including override bindings, must be free before creating UI.
for _,i in ipairs(nums) do
    bindings['CTRL-SHIFT-F'..i]='ACTION'
    assert(not T.Enable(42,deliver,'chord') and not T.frame)
    bindings={}
end
bindings.F13='irrelevant in chord mode'
local link=assert(T.Enable(42,deliver,'chord'))
local frame=T.frame
frame:OnUpdate() assert(link.ready and frame.propagate and link.mode=='chord')
for _,i in ipairs(nums) do assert(checked['CTRL-SHIFT-F'..i]) end
assert(not checked.F13)
local function key(k) frame:OnKeyDown(k) assert(frame.propagate) end
local function ready(expected) frame:OnUpdate() assert(link.ready==expected) end
key('F10') assert(T.keys.buffer==nil) -- unmodified F9 is not a start
ctrl=true key('LCTRL') ready(false)
shift=true key('LSHIFT') ready(true)
local function feed(packet)
    key('F10')
    for i=1,#packet do
        local n=packet:byte(i)
        key('F'..nums[1+math.floor(n/64)])
        key('F'..nums[1+math.floor(n/8)%8])
        key('F'..nums[1+n%8])
    end
    key('F11')
end
local packet=T.Encode(42,1,0,'hello',true)
-- Partial modifier, Meta, edit focus, combat and newly bound chords reset frames.
local blockers={
    {function() shift=false end,function() shift=true end},
    {function() alt=true end,function() alt=false end},
    {function() meta=true end,function() meta=false end},
    {function() focus={} end,function() focus=false end},
    {function() combat=true end,function() combat=false end},
    {function() bindings['CTRL-SHIFT-F10']='ACTION' end,function() bindings['CTRL-SHIFT-F10']=nil end},
}
for _,block in ipairs(blockers) do
    key('F10') key('F1') assert(T.keys.buffer~=nil)
    block[1]() ready(false) assert(T.keys.buffer==nil)
    feed(packet) assert(deliveries==0)
    block[2]() ready(true)
end
key('F10') key('LALT') assert(T.keys.buffer==nil)
key('F10') ctrl,alt,shift=false,false,false ready(true) assert(T.keys.buffer==nil)
ctrl,alt,shift=true,false,true
-- The mapped octal carrier passes the real CRC/session decoder, exactly once.
feed(packet) assert(deliveries==1 and link.rx==1 and T.diagnostics.error=='none')
ctrl,alt,shift=false,false,false ready(true) assert(deliveries==1)
ctrl,alt,shift=true,false,true feed(packet) assert(deliveries==1)
local corrupt=packet:sub(1,-2)..string.char((packet:byte(-1)+1)%256)
feed(corrupt) assert(deliveries==1 and T.diagnostics.error=='length/crc')
key('F21') assert(T.keys.buffer==nil) -- cannot use the legacy carrier in chord mode
assert(T.diagnostics.frames==3 and T.diagnostics.guarded>0 and T.diagnostics.last=='F11')
assert(T.DebugStatus():find('frames 3',1,true))
ctrl,alt,shift=false,false,false T.Disable() assert(not frame.keyboard)
-- Default mode still checks F13..F24 and refuses modifiers.
assert(not T.Enable(42,deliver)) bindings={}
link=assert(T.Enable(42,deliver)) assert(link.mode=='function')
ctrl,alt,shift=true,false,true T.frame:OnUpdate() assert(not link.ready)
ctrl,alt,shift=false,false,false T.Disable()
-- Application dispatch uses the same chord-aware frame readiness.
local received
NS.OnTransportMessage=function(kind,value) received={kind,value} return true end
link=assert(T.Enable(43,nil,'chord')) frame=T.frame ready(true)
ctrl,alt,shift=true,false,true ready(true)
feed(T.Encode(43,1,0,T.MessageChunk('R',1,5,0,'reply'),true))
assert(received[1]=='reply' and received[2]=='reply' and NS.TransportStatus().ready)
ctrl,alt,shift=false,false,false T.Disable()
-- Exercise the actual slash parser without needing to open its UI.
SlashCmdList={}
C_Timer={After=function(delay,fn) assert(delay==0.1) fn() end}
methods.RegisterEvent=function() end
assert(loadfile('addon/Nanocodex/Core.lua'))('Nanocodex',NS)
NS.Notify=function() end
local calls={}
T.Enable=function(session,deliver,mode) calls[#calls+1]={session,mode=mode} assert(deliver==nil) return {} end
local disabled=0 T.Disable=function() disabled=disabled+1 end
for _,input in ipairs({'bridge 42','bridge 43 chord','bridge 44 function'}) do SlashCmdList.NANOCODEXWOW(input) end
assert(#calls==3 and calls[1][1]==42 and calls[1].mode=='chord' and calls[2].mode=='chord' and calls[3].mode=='function')
for _,input in ipairs({'bridge 0 chord','bridge 42 nope','bridge 42 chord extra','bridge 1.5 chord'}) do SlashCmdList.NANOCODEXWOW(input) end
assert(#calls==3)
SlashCmdList.NANOCODEXWOW('bridge off') assert(disabled==1)
print('Lua chord transport: all assertions passed')
