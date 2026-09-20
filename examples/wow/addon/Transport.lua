local ADDON, NS = ...
-- Public UI only. Guarded automatic chord startup; no bindings, focus, networking or gameplay calls.
local T = { MAX_PAYLOAD = 96, MAX_PACKET = 111 }
NS.Transport = T
local floor, char, byte = math.floor, string.char, string.byte
local function xor(a,b)
    local n,p=0,1
    for _=1,16 do if a%2~=b%2 then n=n+p end a=floor(a/2) b=floor(b/2) p=p*2 end
    return n
end
-- WoW provides bit.bxor; the arithmetic fallback keeps stock Lua tests portable.
xor=(type(bit)=='table' and bit.bxor) or xor
local crcTable={}
for n=0,255 do
    local c=n*256
    for _=1,8 do local high=c>=32768 c=(c*2)%65536 if high then c=xor(c,4129) end end
    crcTable[n]=c
end
function T.CRC(s)
    local c=65535
    for i=1,#s do c=xor((c*256)%65536,crcTable[xor(floor(c/256),byte(s,i))]) end
    return c
end
local function u16(n) return char(floor(n/256)%256,n%256) end
local function r16(s,i) return byte(s,i)*256+byte(s,i+1) end
function T.Encode(session,seq,ack,payload,ready)
    payload=payload or ''
    assert(session>=1 and session<=4294967295 and session==floor(session),'session')
    assert(seq>=0 and seq<=65535 and seq==floor(seq) and ack>=0 and ack<=65535 and ack==floor(ack),'sequence')
    assert(#payload<=96 and (seq~=0 or #payload==0),'payload')
    local s='NC1'..char(ready and 1 or 0)..u16(floor(session/65536))..u16(session%65536)..u16(seq)..u16(ack)..char(#payload)..payload
    return s..u16(T.CRC(s))
end
function T.Decode(s)
    if type(s)~='string' or #s<15 or #s>111 or s:sub(1,3)~='NC1' or byte(s,4)>1 then return nil,'header' end
    local n=byte(s,13)
    if n>96 or #s~=15+n or T.CRC(s:sub(1,-3))~=r16(s,#s-1) then return nil,'length/crc' end
    local f={session=r16(s,5)*65536+r16(s,7),seq=r16(s,9),ack=r16(s,11),payload=s:sub(14,-3),ready=byte(s,4)==1}
    if f.session==0 or (f.seq==0 and n~=0) then return nil,'fields' end
    return f
end
function T.New(session,deliver)
    local self={session=session,tx=0,rx=0,pending=nil,deliver=deliver,ready=false,acked=0,peerSerial=0}
    function self:Send(payload)
        if type(payload)~='string' or #payload>96 then return nil,'payload limit' end
        if self.pending then return nil,'busy' end
        if self.tx==65535 then return nil,'new session required' end
        self.tx=self.tx+1 self.pending=payload return self.tx
    end
    function self:Packet()
        local seq=self.pending and self.tx or 0
        if self.cached and self.cachedSeq==seq and self.cachedAck==self.rx and self.cachedReady==self.ready and self.cachedPayload==self.pending then return self.cached end
        self.cached=T.Encode(self.session,seq,self.rx,self.pending or '',self.ready)
        self.cachedSeq,self.cachedAck,self.cachedReady,self.cachedPayload=seq,self.rx,self.ready,self.pending
        return self.cached
    end
    function self:Receive(s)
        local f,err=T.Decode(s)
        if not f then return nil,err end
        if f.session~=self.session then return nil,'session' end
        if f.ack>self.tx then return nil,'future ack' end
        if f.seq~=0 and f.seq~=self.rx and f.seq~=self.rx+1 then return nil,'sequence gap' end
        if f.seq~=0 and f.seq==self.rx and f.payload~=self.last then return nil,'conflicting duplicate' end
        if f.seq~=0 and f.seq==self.rx+1 then
            -- Handler accepts data only; it must never dispatch arbitrary Lua or gameplay actions.
            if self.deliver then local ok,accepted=pcall(self.deliver,f.payload,f.seq) if not ok or accepted==false then return nil,'delivery rejected' end end
            self.rx=f.seq self.last=f.payload
        end
        if self.pending and f.ack==self.tx then self.pending=nil end
        self.acked=math.max(self.acked,f.ack) self.peerSerial=self.peerSerial+1
        return true
    end
    return self
end
function T.NewKeys(receive,clock)
    local self={buffer=nil,digits='',mode=nil,started=0,last=0}
    function self:Reset() self.buffer=nil self.digits='' self.mode=nil end
    function self:Key(key)
        local n=tonumber(key:match('^F(%d+)$'))
        if not n or n<13 or n>24 then return false end
        local now=clock()
        if self.buffer and (now-self.last>2 or now-self.started>30) then self:Reset() end
        self.last=now
        if self.mode=='binary' then
            if n==24 then
                local s=self.buffer local complete=self.digits=='' self:Reset()
                if complete then return true,receive(s) end
            elseif n==19 or n==23 then
                -- Reject the first excess bit, keeping the whole carrier <=890 keys.
                if #self.buffer>=T.MAX_PACKET then self:Reset() return true end
                self.digits=self.digits..(n==19 and '0' or '1')
                if #self.digits==8 then
                    self.buffer=self.buffer..char(tonumber(self.digits,2)) self.digits=''
                end
            else
                -- Never reinterpret an octal marker as a restart inside binary.
                self:Reset()
            end
        elseif n==24 then
            if self.buffer then self:Reset()
            else self.buffer='' self.digits='' self.mode='binary' self.started=now end
        elseif n==21 then self.buffer='' self.digits='' self.mode='octal' self.started=now
        elseif n==22 then
            local s=self.buffer local complete=self.digits=='' self:Reset()
            if s and complete then return true,receive(s) end
        elseif n==23 then self:Reset()
        elseif self.buffer then
            self.digits=self.digits..tostring(n-13)
            if #self.digits==3 then
                local v=tonumber(self.digits,8) self.digits=''
                if v>255 or #self.buffer>=T.MAX_PACKET then self:Reset() else self.buffer=self.buffer..char(v) end
            end
        end
        return true
    end
    return self
end
-- Application messages inside NC1: M, kind, u16 id, u16 total, u16 offset, <=88 bytes.
T.MAX_MESSAGE=16384
local kinds={Q='request',R='reply',P='projects',A='ack',E='error',S='stream'}
function T.MessageChunk(kind,id,total,offset,text)
    assert(kinds[kind] and id>=1 and id<=65535 and id==floor(id),'message identity')
    assert(total>=0 and total<=T.MAX_MESSAGE and total==floor(total),'message length')
    assert(offset>=0 and offset==floor(offset) and offset+#text<=total and #text<=88,'chunk length')
    assert(#text>0 or (total==0 and offset==0),'empty chunk')
    return 'M'..kind..u16(id)..u16(total)..u16(offset)..text
end
function T.NewApplication(link,clock,dispatch)
    assert(not link.deliver,'application requires an unused receive callback')
    local app={link=link,clock=clock,dispatch=dispatch,nextID=0,lastID=0,peerSerial=link.peerSerial}
    function app:Incoming(payload)
        if #payload<8 or #payload>96 or payload:sub(1,1)~='M' then return false end
        local kind=payload:sub(2,2)
        -- This side only accepts daemon-to-addon message kinds.
        if not kinds[kind] or kind=='Q' then return false end
        local id,total,offset=r16(payload,3),r16(payload,5),r16(payload,7)
        local text=payload:sub(9)
        if id~=self.lastID+1 or total>T.MAX_MESSAGE or offset+#text>total or (#text==0 and total~=0) then return false end
        local incoming=self.incoming
        if incoming then
            if id~=incoming.id or kind~=incoming.kind or total~=incoming.total or offset~=incoming.size then return false end
        elseif offset~=0 then return false end
        local prefix=incoming and incoming.text or ''
        local combined=prefix..text
        if #combined==total then
            if type(self.dispatch)~='function' then self.receiveError='message handler unavailable' return false end
            local ok,accepted=pcall(self.dispatch,kinds[kind],combined)
            if not ok or accepted==false then self.receiveError='message handler rejected delivery' return false end
            self.lastID=id self.incoming=nil self.receiveError=nil
        else
            self.incoming={id=id,kind=kind,total=total,size=#combined,text=combined}
        end
        return true
    end
    link.deliver=function(payload) return app:Incoming(payload) end
    function app:Update()
        if self.peerSerial~=link.peerSerial then self.peerSerial=link.peerSerial self.lastPeer=clock() end
        local request=self.request
        if not request or request.done then return end
        if request.awaitSeq and link.acked>=request.awaitSeq then
            request.offset=request.sentEnd request.awaitSeq=nil
            if request.offset==#request.text then
                request.done=true
                -- Delivery here means peer transport accepted every chunk, NOT model success.
                if type(self.dispatch)=='function' then
                    local ok=pcall(self.dispatch,'transport_ack','NC1 request '..request.id..' acknowledged by peer transport')
                    if not ok then self.receiveError='ack notification failed' end
                end
                return
            end
        end
        if not request.awaitSeq and not link.pending then
            local chunk=request.text:sub(request.offset+1,request.offset+88)
            local seq,err=link:Send(T.MessageChunk('Q',request.id,#request.text,request.offset,chunk))
            if not seq then self.sendError=err return end
            request.awaitSeq=seq request.sentEnd=request.offset+#chunk
        end
    end
    function app:Send(payload,newRequest)
        if type(payload)~='string' or #payload>T.MAX_MESSAGE then return false,'payload limit' end
        self:Update()
        local request=self.request
        if not newRequest and request and request.text==payload then
            if request.done then return true end
            return false,'pending'
        end
        if (request and not request.done) or link.pending then return false,'busy' end
        local chunks=math.max(1,math.ceil(#payload/88))
        if self.nextID==65535 or link.tx+chunks>65535 then return false,'new session required' end
        self.nextID=self.nextID+1 self.sendError=nil
        self.request={id=self.nextID,text=payload,offset=0,done=false}
        self:Update()
        return false,'pending'
    end
    function app:Status()
        self:Update()
        local request=self.request
        local connected=self.lastPeer~=nil and clock()-self.lastPeer<=10
        local state=request and (request.done and 'acknowledged' or 'pending') or 'idle'
        return {connected=connected,state=state,pending=request~=nil and not request.done,
            acknowledged=request~=nil and request.done or false,ready=link.ready,
            session=link.session,sequence=link.tx,ack=link.acked,received=link.rx,
            message_id=request and request.id or nil,bytes_acked=request and request.offset or 0,
            bytes_total=request and #request.text or 0,
            error=self.sendError or self.receiveError,
            message=state=='pending' and 'Awaiting peer transport ACK; backend acceptance unknown' or
                (state=='acknowledged' and 'Peer transport ACK received; backend/model result unconfirmed' or
                (connected and 'Peer transport observed; backend/model unconfirmed' or 'No recent peer transport frame'))}
    end
    return app
end
local function saved()
    if type(NanocodexWowDB)~='table' then NanocodexWowDB={} end
    return NanocodexWowDB
end
local function validSession(session)
    return type(session)=='number' and session>=1 and session<=4294967295 and session==floor(session)
end
local function freshSession()
    local previous=saved().bridgeLastSession
    -- Persist a monotonic identifier across UI reloads. This is a replay boundary,
    -- not an authentication token. Do not reseed WoW's shared random generator.
    if validSession(previous) then return previous%4294967295+1 end
    local epoch=type(GetServerTime)=='function' and GetServerTime() or 0
    return floor((epoch*1000+GetTime()*1000)%4294967295)+1
end
local chordNumbers={1,2,3,5,6,7,8,9,10,11}
local chordSymbols={}
for i,n in ipairs(chordNumbers) do chordSymbols[n]=i+12 end
local modifierAPIs={'IsControlKeyDown','IsAltKeyDown','IsShiftKeyDown','IsMetaKeyDown'}
local function chordModifiers()
    for _,name in ipairs(modifierAPIs) do
        if type(_G[name])~='function' then return nil,'missing API: '..name end
    end
    -- Blizzard's 1.60.1 InputDocumentation and SharedXML/KeyCommand expose and
    -- use IsMetaKeyDown (wow-ui-source 70ef1b2fd78061a73f886c4a1e79dc5b5cff6d5e).
    -- An absent API is an explicit guard, never assumed false.
    local ctrl,alt,shift,meta=IsControlKeyDown(),IsAltKeyDown(),IsShiftKeyDown(),IsMetaKeyDown()
    if meta then return nil,'Meta modifier is held' end
    if alt then return nil,'Alt modifier is held' end
    if ctrl and shift then return 'chord' end
    if not ctrl and not shift then return 'idle' end
    return nil,'partial chord: hold both Ctrl and Shift, or release both'
end
local function unbound(key)
    if (GetBindingAction(key,true) or '')~='' then return false,'binding conflict: '..key end
    return true
end
local function available(mode)
    if T.stopRequested then return false,T.stopReason or 'bridge off requested; waiting for safe keyboard teardown' end
    if T.lifecycle and not T.inWorld then return false,'waiting for PLAYER_ENTERING_WORLD' end
    if type(InCombatLockdown)~='function' then return false,'missing API: InCombatLockdown' end
    if InCombatLockdown() then return false,'combat lockdown' end
    if type(GetCurrentKeyBoardFocus)~='function' then return false,'missing API: GetCurrentKeyBoardFocus' end
    if GetCurrentKeyBoardFocus() then return false,'keyboard focus is held by an edit box' end
    if mode=='chord' then
        local modifiers,reason=chordModifiers()
        if not modifiers then return false,reason end
    elseif mode=='function' then
        if type(IsModifierKeyDown)~='function' then return false,'missing API: IsModifierKeyDown' end
        if IsModifierKeyDown() then return false,'a modifier is held' end
    else return false,'unknown receiver mode' end
    if type(GetBindingAction)~='function' then return false,'missing API: GetBindingAction' end
    if mode=='chord' then
        for _,n in ipairs(chordNumbers) do
            local ok,reason=unbound('CTRL-SHIFT-F'..n)
            if not ok then return false,reason end
        end
    else
        for n=13,24 do
            local ok,reason=unbound('F'..n)
            if not ok then return false,reason end
        end
    end
    if type(GetTime)~='function' then return false,'missing API: GetTime' end
    if type(CreateFrame)~='function' then return false,'missing API: CreateFrame' end
    return true
end
T.GuardStatus=available
function NS.TransportSend(payload,newRequest)
    if T.stopRequested then return false,'bridge is disabled' end
    if not T.app then return false,NS.TransportStatus().message end
    return T.app:Send(payload,newRequest)
end
function NS.TransportStatus()
    local status=T.app and T.app:Status() or
        {connected=false,state='disabled',pending=false,acknowledged=false,ready=false,message='Transport unavailable'}
    status.mode=T.link and T.link.mode or 'chord'
    status.auto_start=type(NanocodexWowDB)~='table' or NanocodexWowDB.bridgeAutoStart~=false
    if T.stopRequested then
        status.connected=false status.ready=false status.state='stopping'
        status.guard_reason=T.stopReason or 'bridge off requested; waiting for safe keyboard teardown'
        status.message=status.guard_reason
    elseif not T.frame and not status.auto_start then
        status.message='Bridge disabled; automatic startup is off'
    elseif T.frame then
        local ready,reason=available(status.mode)
        status.ready=ready status.guard_reason=reason
        if not ready then status.connected=false end
        status.message=(ready and ('Bridge '..status.mode..' session '..T.link.session..'; ') or
            ('Bridge paused: '..reason..'; '))..status.message
    elseif T.lifecycle then
        local reason
        if not T.loaded then reason='waiting for ADDON_LOADED'
        elseif T.startupError then reason=T.startupError
        else local _,guard=available('chord') reason=guard or 'waiting for automatic startup' end
        status.state='waiting' status.guard_reason=reason status.message='Bridge waiting: '..reason
    end
    return status
end
function T.Enable(session,deliver,mode)
    mode=mode or 'function'
    if mode~='function' and mode~='chord' then return nil,'unknown receiver mode' end
    if T.frame then return nil,'already enabled' end
    local ready,reason=available(mode)
    if not ready then return nil,reason end
    -- Validate before creating any UI.
    if not validSession(session) then return nil,'session must be a nonzero 32-bit integer' end
    local frame=CreateFrame('Frame',nil,UIParent)
    for _,name in ipairs({'SetPropagateKeyboardInput','EnableKeyboard'}) do
        if type(frame[name])~='function' then frame:Hide() return nil,'missing frame API: '..name end
    end
    frame:SetSize(128,128) frame:SetPoint('TOPLEFT',UIParent,'TOPLEFT',16,-16) -- Avoid compositor rounded-window clipping.
    frame:SetFrameStrata('TOOLTIP') frame:EnableMouse(false)
    frame:SetPropagateKeyboardInput(true) frame:EnableKeyboard(true)
    local cells={}
    for i=1,1024 do
        local cell=frame:CreateTexture(nil,'OVERLAY')
        cell:SetSize(4,4) cell:SetPoint('TOPLEFT',(i-1)%32*4,-floor((i-1)/32)*4)
        cells[i]=cell
    end
    T.diagnostics={keys=0,reserved=0,frames=0,guarded=0,last='none',error='none'}
    local link=T.New(session,deliver)
    link.mode=mode
    local app
    if not deliver then
        app=T.NewApplication(link,GetTime,function(kind,text)
            if type(NS.OnTransportMessage)~='function' then return false end
            return NS.OnTransportMessage(kind,text)
        end)
    end
    local keys=T.NewKeys(function(s)
        local ok,err=link:Receive(s)
        T.diagnostics.frames=T.diagnostics.frames+1
        T.diagnostics.error=ok and 'none' or tostring(err)
        if app then app:Update() end
        return ok,err
    end,GetTime)
    local previous
    local function paint()
        local packet=link:Packet()
        if previous==packet then return end
        -- Only changed cells; at most one presentation per rendered game frame.
        for n=1,128 do
            local b=byte(packet,n) or 0
            local old=previous and (byte(previous,n) or 0)
            if b~=old then
                for j=0,7 do
                    local v=floor(b/2^(7-j))%2
                    if not old or v~=floor(old/2^(7-j))%2 then cells[(n-1)*8+j+1]:SetColorTexture(v,v,v,1) end
                end
            end
        end
        previous=packet
    end
    frame:SetScript('OnKeyDown',function(_,key)
        -- Always propagate. No overrides, no consuming gameplay input, no SetFocus.
        T.diagnostics.keys=T.diagnostics.keys+1
        local n=tonumber(key:match('^F(%d+)$'))
        local carrier=n and ((mode=='chord' and chordSymbols[n]~=nil) or (mode=='function' and n>=13 and n<=24))
        if carrier then T.diagnostics.reserved=T.diagnostics.reserved+1 T.diagnostics.last=key end
        local ready,reason=available(mode)
        T.guardReason=reason link.ready=ready
        if not ready then T.diagnostics.guarded=T.diagnostics.guarded+1 keys:Reset() return end
        if mode=='chord' then
            -- Idle is ready for a burst, but only the exact held chord carries symbols.
            -- Modifier presses and unrelated keys cannot continue an incomplete frame.
            if chordModifiers()~='chord' or not carrier then keys:Reset() return end
            key='F'..chordSymbols[n]
        end
        keys:Key(key) -- OnUpdate presents completed packets; never redraw per symbol.
    end)
    frame:SetScript('OnUpdate',function()
        link.ready,T.guardReason=available(mode)
        if not link.ready or (mode=='chord' and chordModifiers()~='chord') then keys:Reset() end
        if app then app:Update() end paint()
    end)
    T.frame,T.link,T.keys,T.app=frame,link,keys,app
    saved().bridgeLastSession=session saved().bridgeAutoStart=true
    T.startupError=nil T.guardReason=nil link.ready=true
    paint()
    return link
end
local function finishDisable()
    if not T.frame then T.stopRequested=nil T.stopReason=nil return true end
    if type(InCombatLockdown)~='function' then
        T.stopReason='missing API: InCombatLockdown; keyboard teardown deferred'
        return nil,T.stopReason
    end
    if InCombatLockdown() then
        T.stopReason='bridge off saved; keyboard teardown deferred until combat ends'
        return nil,T.stopReason
    end
    T.frame:EnableKeyboard(false) T.frame:Hide()
    T.frame:SetScript('OnUpdate',nil) T.frame:SetScript('OnKeyDown',nil)
    T.frame,T.link,T.keys,T.app=nil,nil,nil,nil
    T.stopRequested=nil T.stopReason=nil T.guardReason=nil
    return true
end
function T.Disable()
    -- Save the opt-out even while startup is guarded or the receiver is in combat.
    -- Input is ignored immediately; protected frame changes wait for combat to end.
    saved().bridgeAutoStart=false T.stopRequested=true
    if T.link then T.link.ready=false end
    if T.keys then T.keys:Reset() end
    return finishDisable()
end

function T.DebugStatus()
    local status=NS.TransportStatus()
    local d=T.diagnostics
    if not d then return status.message end
    return status.message..('; Keys %d; reserved %d; last %s; frames %d; guarded %d; error %s'):format(
        d.keys,d.reserved,d.last,d.frames,d.guarded,d.error)
end

-- No keyboard listener or carrier is created until SavedVariables are loaded,
-- the player is in-world, and every public input guard permits chord reception.
-- Poll only the blocked startup/teardown at 4 Hz. Never restart an active session
-- (including pending/uncertain requests) because its peer disappeared.
local function startup()
    if T.stopRequested then finishDisable() end
    if not T.loaded or not T.inWorld or T.frame or saved().bridgeAutoStart==false or T.startupError then return end
    if not available('chord') then return end
    local link,reason=T.Enable(freshSession(),nil,'chord')
    if not link then T.startupError=reason end
end
-- Explicit user opt-in. Polling startup waits for chat focus, combat and binding
-- guards to clear; repeated opt-in preserves any live session and pending bytes.
function T.StartAutomatic()
    if T.stopRequested then return false,'bridge teardown pending; wait until combat ends' end
    saved().bridgeAutoStart=true
    T.startupError=nil
    startup()
    return true,NS.TransportStatus().message
end
if type(CreateFrame)=='function' then
    local lifecycle=CreateFrame('Frame')
    T.lifecycle=lifecycle
    lifecycle:RegisterEvent('ADDON_LOADED')
    lifecycle:RegisterEvent('PLAYER_ENTERING_WORLD')
    lifecycle:RegisterEvent('PLAYER_LEAVING_WORLD')
    lifecycle:SetScript('OnEvent',function(_,event,name)
        if event=='ADDON_LOADED' and name==ADDON then T.loaded=true
        elseif event=='PLAYER_ENTERING_WORLD' then T.inWorld=true
        elseif event=='PLAYER_LEAVING_WORLD' then
            T.inWorld=false
            if T.link then T.link.ready=false end
            if T.keys then T.keys:Reset() end
        else return end
        startup()
    end)
    local elapsed=0
    lifecycle:SetScript('OnUpdate',function(_,delta)
        elapsed=elapsed+delta
        if elapsed<0.25 then return end
        elapsed=0 startup()
    end)
end
