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
-- Python compares this Lua-produced envelope with its encoder.
print((T.MessageChunk('R',1,#reply,88,reply:sub(89)):gsub('.',function(c) return string.format('%02x',c:byte()) end)))
print('Lua application contract: all assertions passed')
