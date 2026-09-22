local NS={}
assert(loadfile('addon/Transport.lua'))('Nanocodex',NS)
local T=NS.Transport
local now,events=0,{}
local link=T.New(42)
local app=T.NewApplication(link,function() return now end,function(kind,text) events[#events+1]={kind,text} end)
T.app=app
local status=NS.TransportStatus() assert(not status.connected and not status.acknowledged)
local payload=string.rep('A',89)..'\195\169'
local ok,err=NS.TransportSend(payload) assert(ok==false and err=='pending')
assert(link.tx==1 and #link.pending==96)
ok,err=NS.TransportSend(payload) assert(not ok and err=='pending' and link.tx==1)
ok,err=NS.TransportSend('other') assert(not ok and err=='busy')
assert(not NS.TransportStatus().connected)
assert(link:Receive(T.Encode(42,0,1,'',true)))
status=NS.TransportStatus() assert(status.connected and status.pending and status.bytes_acked==88 and not status.acknowledged)
assert(link.tx==2)
-- Duplicate old ACK cannot complete the final chunk.
assert(link:Receive(T.Encode(42,0,1,'',true)))
ok,err=NS.TransportSend(payload) assert(not ok and err=='pending')
assert(#events==0)
assert(link:Receive(T.Encode(42,0,2,'',true)))
ok,err=NS.TransportSend(payload) assert(ok==true and err==nil)
status=NS.TransportStatus() assert(status.acknowledged and not status.pending and status.bytes_acked==#payload)
assert(#events==1 and events[1][1]=='transport_ack')
assert(NS.TransportSend(payload)==true and link.tx==2 and #events==1)
now=11 assert(not NS.TransportStatus().connected)
-- Incoming chunks dispatch once, preserving binary/UTF8 strings exactly.
local reply=string.rep('R',87)..'\195\169'
local c1=T.MessageChunk('R',1,#reply,0,reply:sub(1,88))
local c2=T.MessageChunk('R',1,#reply,88,reply:sub(89))
assert(link:Receive(T.Encode(42,1,2,c1,true))) assert(#events==1)
assert(link:Receive(T.Encode(42,1,2,c1,true))) assert(#events==1)
assert(link:Receive(T.Encode(42,2,2,c2,true))) assert(#events==2)
assert(events[2][1]=='reply' and events[2][2]==reply)
assert(link:Receive(T.Encode(42,2,2,c2,true))) assert(#events==2)
-- Wrong kind, gaps, over-bound totals and replayed IDs are not acknowledged.
assert(not link:Receive(T.Encode(42,3,2,T.MessageChunk('Q',2,1,0,'x'),true)))
assert(not link:Receive(T.Encode(42,3,2,T.MessageChunk('R',2,2,1,'x'),true)))
assert(not link:Receive(T.Encode(42,3,2,c2,true)))
local malicious='MR'..string.char(0,2,255,255,0,0)..'x'
assert(not link:Receive(T.Encode(42,3,2,malicious,true)))
assert(link.rx==2)
assert(not NS.TransportSend(string.rep('x',16385)))
-- Reject final callback without losing prefix; retry final accepted exactly once.
local reject=true
local l2=T.New(9)
local a2=T.NewApplication(l2,function() return now end,function() return not reject end)
local first=T.MessageChunk('P',1,89,0,string.rep('x',88))
local last=T.MessageChunk('P',1,89,88,'y')
assert(l2:Receive(T.Encode(9,1,0,first,true)))
assert(not l2:Receive(T.Encode(9,2,0,last,true))) assert(l2.rx==1 and a2.lastID==0 and a2.incoming.size==88)
reject=false assert(l2:Receive(T.Encode(9,2,0,last,true))) assert(a2.lastID==1 and not a2.incoming)
-- Streaming is a separate application kind, rendered after each bounded message.
local received={}
local sl=T.New(7)
local sa=T.NewApplication(sl,function() return now end,function(kind,text) received[#received+1]={kind,text} end)
for id,text in ipairs({'partial one','partial two','completed'}) do
    assert(sl:Receive(T.Encode(7,id,0,T.MessageChunk('S',id,#text,0,text),true)))
    assert(#received==id and received[id][1]=='stream' and received[id][2]==text)
end
-- Empty message, ID and transport sequence exhaustion.
local l3=T.New(9) local a3=T.NewApplication(l3,function() return now end,function() end)
assert(not a3:Send('')) assert(l3.tx==1 and #l3.pending==8)
assert(l3:Receive(T.Encode(9,0,1,'',true))) assert(a3:Send(''))
l3.tx=65535 assert(select(2,a3:Send('next'))=='new session required')
T.app=nil assert(not NS.TransportSend('x') and NS.TransportStatus().state=='disabled')
-- Explicit operations queue with admission-time identities while bytes stay FIFO.
do
    local qlink=T.New(123)
    local receipts,parts={},{}
    local qapp=T.NewApplication(qlink,function() return now end,function(kind,text)
        receipts[#receipts+1]={kind,text}
    end)
    local peer=T.New(123,function(part) parts[#parts+1]=part return true end)
    local first=string.rep('a',89)
    assert(select(2,qapp:Send(first,true))=='pending')
    local firstRequest=qapp.request
    local originalPacket=qlink:Packet()
    assert(select(2,qapp:Send('second',true))=='pending')
    local secondRequest=qapp.request
    assert(select(2,qapp:Send('second',true))=='pending')
    local thirdRequest=qapp.request
    assert(firstRequest.id==1 and secondRequest.id==2 and thirdRequest.id==3)
    local queued=qapp:Status()
    assert(queued.message_id==3 and queued.active_message_id==1)
    assert(queued.pending_requests==3 and queued.bytes_acked==0 and queued.bytes_total==6)
    assert(queued.pending and not queued.acknowledged and queued.queue_limit==T.MAX_REQUESTS)
    -- Game-mode correlation must bind the latest admission, not the active bytes.
    local clientNS={TransportStatus=function() return qapp:Status() end,Reply=function() return true end}
    assert(loadfile('addon/Nanocodex/Client.lua'))('Nanocodex',clientNS)
    clientNS.ClientGameSent()
    assert(clientNS.ReceiveClientMessage('ack','ncm1\tsend\tncw:0000007b:0001\tolder-thread\tturn-1\tremoteaccepted'))
    assert(clientNS.ClientState().thread==nil)
    assert(clientNS.ReceiveClientMessage('ack','ncm1\tsend\tncw:0000007b:0003\tnewer-thread\tturn-3\tremoteaccepted'))
    assert(clientNS.ClientState().thread=='newer-thread')
    assert(qlink:Packet()==originalPacket, 'admission must not replace unacked bytes')
    assert(select(2,qapp:Send('second'))=='pending' and qapp.nextID==3)
    assert(select(2,qapp:Send('other'))=='busy' and qapp.nextID==3)
    for _=1,4 do qapp:Status() end
    assert(qlink.tx==1 and #receipts==0, 'status must not resubmit operations')
    -- A lost ACK repeats exactly the same carrier packet and application ID.
    assert(peer:Receive(originalPacket)) assert(peer:Receive(qlink:Packet()))
    assert(#parts==1 and qlink.tx==1)
    assert(qlink:Receive(peer:Packet()))
    queued=qapp:Status()
    assert(qlink.tx==2 and queued.active_message_id==1 and queued.bytes_acked==0)
    assert(firstRequest.offset==88 and not firstRequest.done and #receipts==0)
    assert(qlink:Receive(T.Encode(123,0,1,'',true)))
    qapp:Update() assert(not firstRequest.done and qlink.tx==2)
    assert(peer:Receive(qlink:Packet())) assert(qlink:Receive(peer:Packet()))
    queued=qapp:Status()
    assert(firstRequest.done and not secondRequest.done and not thirdRequest.done)
    assert(queued.message_id==3 and queued.active_message_id==2 and queued.pending_requests==2)
    assert(queued.pending and not queued.acknowledged and #receipts==1 and qlink.tx==3)
    -- Replies can arrive while unrelated outbound requests are still pending.
    assert(peer:Send(T.MessageChunk('R',1,5,0,'reply')))
    assert(qlink:Receive(peer:Packet()))
    assert(receipts[2][1]=='reply' and receipts[2][2]=='reply')
    while qapp:Status().pending do
        assert(peer:Receive(qlink:Packet())) assert(qlink:Receive(peer:Packet()))
    end
    queued=qapp:Status()
    assert(secondRequest.done and thirdRequest.done and queued.message_id==3)
    assert(queued.active_message_id==nil and queued.pending_requests==0)
    assert(queued.acknowledged and queued.bytes_acked==6 and qapp.reservedChunks==0)
    assert(#parts==4 and parts[1]==T.MessageChunk('Q',1,89,0,first:sub(1,88)))
    assert(parts[2]==T.MessageChunk('Q',1,89,88,'a'))
    assert(parts[3]==T.MessageChunk('Q',2,6,0,'second'))
    assert(parts[4]==T.MessageChunk('Q',3,6,0,'second'))
    assert(#receipts==4 and receipts[3][2]=='NC1 request 2 acknowledged by peer transport')
    assert(receipts[4][2]=='NC1 request 3 acknowledged by peer transport')
    assert(qapp:Send('second') and qapp.nextID==3 and qlink.tx==4)
end
-- Bound both active and waiting work; reject without consuming an identity.
do
    local qlink=T.New(124)
    local qapp=T.NewApplication(qlink,function() return now end,function() end)
    for i=1,T.MAX_REQUESTS do
        assert(select(2,qapp:Send(string.rep('x',T.MAX_MESSAGE),true))=='pending')
        assert(qapp.request.id==i)
    end
    local packet=qlink:Packet()
    assert(select(2,qapp:Send('overflow',true))=='busy')
    assert(qapp.nextID==T.MAX_REQUESTS and qapp:Status().pending_requests==T.MAX_REQUESTS)
    assert(qlink:Packet()==packet and qapp.request.id==T.MAX_REQUESTS)
    assert(select(2,qapp:Send(qapp.request.text))=='pending', 'legacy retry still works at capacity')
    local peer=T.New(124,function() return true end)
    while qapp.outgoing[1].id==1 do
        assert(peer:Receive(qlink:Packet())) assert(qlink:Receive(peer:Packet())) qapp:Update()
    end
    assert(select(2,qapp:Send('after capacity frees',true))=='pending')
    assert(qapp.request.id==T.MAX_REQUESTS+1 and qapp:Status().pending_requests==T.MAX_REQUESTS)
end
-- Reserve sequence space for queued chunks, including an empty message.
do
    local qlink=T.New(125) qlink.tx=65532
    local qapp=T.NewApplication(qlink,function() return now end,function() end)
    assert(select(2,qapp:Send(string.rep('x',89),true))=='pending')
    assert(qlink.tx==65533 and qapp.reservedChunks==1)
    assert(select(2,qapp:Send('',true))=='pending')
    assert(qapp.reservedChunks==2 and qapp.nextID==2)
    assert(select(2,qapp:Send('overflow',true))=='new session required')
    assert(qapp.nextID==2 and qapp.request.id==2 and qapp.reservedChunks==2)
    while qapp:Status().pending do
        assert(qlink:Receive(T.Encode(125,0,qlink.tx,'',true))) qapp:Update()
    end
    assert(qlink.tx==65535 and qapp.reservedChunks==0 and qapp.request.done)
    assert(select(2,qapp:Send('overflow',true))=='new session required')
    assert(qapp.nextID==2)
    local idlink=T.New(126)
    local idapp=T.NewApplication(idlink,function() return now end,function() end)
    idapp.nextID=65534
    assert(select(2,idapp:Send('last',true))=='pending' and idapp.request.id==65535)
    assert(select(2,idapp:Send('overflow',true))=='new session required')
    assert(idapp.nextID==65535 and idapp:Status().pending_requests==1)
end
-- Python compares this Lua-produced envelope with its encoder.
print((T.MessageChunk('R',1,#reply,88,reply:sub(89)):gsub('.',function(c) return string.format('%02x',c:byte()) end)))
print('Lua application contract: all assertions passed')
