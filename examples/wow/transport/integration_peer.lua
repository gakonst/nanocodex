-- Deterministic addon peer for Python integration tests. No WoW or desktop APIs.
local NS={Notify=function() end}
assert(loadfile('addon/Nanocodex/Context.lua'))('Nanocodex',NS)
assert(loadfile('addon/Nanocodex/Projects.lua'))('Nanocodex',NS)
assert(loadfile('addon/Nanocodex/Client.lua'))('Nanocodex',NS)
assert(loadfile('addon/Nanocodex/Bridge.lua'))('Nanocodex',NS)
assert(loadfile('addon/Transport.lua'))('Nanocodex',NS)
local T=NS.Transport
local display=''
NS.Snapshot=function() NanocodexWowDB.lastContext={schemaVersion=1} return true end
NS.DisplayConversation=function(text) display=text end
-- Real receiver frame/guard and painter, with only public WoW APIs stubbed.
local ctrl,shift=false,false
UIParent={}
InCombatLockdown=function() return false end
GetCurrentKeyBoardFocus=function() return nil end
IsControlKeyDown=function() return ctrl end
IsShiftKeyDown=function() return shift end
IsAltKeyDown=function() return false end
IsMetaKeyDown=function() return false end
GetBindingAction=function() return '' end
GetTime=function() return 1 end
local cells={}
CreateFrame=function()
    local frame={}
    for _,name in ipairs({'SetSize','SetPoint','SetFrameStrata','EnableMouse','Hide','EnableKeyboard'}) do
        frame[name]=function() end
    end
    frame.SetPropagateKeyboardInput=function(_,value) assert(value==true) end
    frame.SetScript=function(self,name,fn) self[name]=fn end
    frame.CreateTexture=function()
        local i=#cells+1 cells[i]=0
        return {SetSize=function() end,SetPoint=function() end,
            SetColorTexture=function(_,r,g,b,a) assert(r==g and g==b and a==1) cells[i]=r end}
    end
    return frame
end
local link=assert(T.Enable(17,nil,'chord'))
local numbers={1,2,3,5,6,7,8,9,10,11}
local request={schemaVersion=1,source='nanocodex-wow',type='nanocodex.ask',mode='hint',prompt='same explicit question'}
for command in io.lines() do
    local submitted,reason
    if command=='ask' then submitted,reason=NS.QueueRequest(request)
    elseif command=='refresh' then submitted=NS.RefreshWorkspace()
    elseif command=='select1' then submitted=NS.LoadThread('fixture-project','fixture-thread')
    elseif command=='select2' then submitted=NS.LoadThread('fixture-project','fixture-other')
    elseif command=='earlier' then submitted=NS.LoadEarlier()
    elseif command=='sendthread' then submitted=NS.Ask('Native thread request')
    elseif command=='new' then submitted=NS.ProjectAction('create_chat','New conversation')
    elseif command=='stop' then submitted=NS.StopThread()
    elseif command=='reconnect' then submitted=NS.ReconnectClient()
    elseif command:sub(1,5)=='keys ' then
        ctrl=true T.frame:OnKeyDown('LCTRL')
        shift=true T.frame:OnKeyDown('LSHIFT')
        for key in command:sub(6):gmatch('%S+') do
            local index=assert(tonumber(key:match('^C(%d+)$')))
            T.frame:OnKeyDown('F'..assert(numbers[index]))
            T.frame:OnUpdate() -- Render ticks during a native chord burst.
        end
        ctrl,shift=false,false
        T.frame:OnUpdate()
    else assert(command=='capture') end
    T.frame:OnUpdate()
    NS.PumpClient()
    NS.FlushConversation(.05)
    local status=NS.TransportStatus()
    print(NS.Encode({packet=(link:Packet():gsub('.',function(c) return ('%02x'):format(c:byte()) end)),
        cells=table.concat(cells),frames=T.diagnostics.frames,text=display,status=NS.StreamState().status,transport=NS.TransportDisplay(),
        client=NS.ClientState(),threads=#NS.ThreadEntries(),pending=status.pending,mid=status.message_id,submitted=submitted,reason=reason,
        incoming_bytes=T.app.incoming and T.app.incoming.size or 0,received_mid=T.app.lastID}))
    io.stdout:flush()
end
