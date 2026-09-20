local NS={}
local shown,focus,notices,display=0,0,{},''
NS.Notify=function(_,text) notices[#notices+1]=text end
NS.DisplayConversation=function(text) display=text end
NS.OpenConversation=function() shown=shown+1 end
assert(loadfile('addon/Nanocodex/Bridge.lua'))('Nanocodex',NS)
local function frame(rev,offset,op,text,sid)
 return 'ncs1\tr\tt\t'..(sid or 's')..'\t'..rev..'\t'..offset..'\t'..op..'\n'..(text or '')
end
assert(NS.ReceiveStream(frame(1,0,'append','Hello ')))
NS.FlushConversation(.05) assert(display=='Hello ' and shown==0)
assert(NS.StreamState().status=='Streaming reply…')
assert(NS.ReceiveStream(frame(1,0,'append','Hello '))) -- Exact replay.
assert(not NS.ReceiveStream(frame(1,0,'append','Wrong ')))
assert(not NS.ReceiveStream(frame(3,6,'append','gap')))
assert(not NS.ReceiveStream(frame(2,5,'append','offset')))
assert(NS.ReceiveStream(frame(2,6,'append','\195\169|')))
NS.FlushConversation(.05) assert(display=='Hello \195\169||')
assert(not NS.ReceiveStream(frame(3,9,'append','\195'))) -- Incomplete UTF8.
assert(NS.ReceiveStream(frame(3,9,'end')))
assert(NS.StreamState().status~='Completed')
assert(NS.ReceiveStream(frame(4,9,'done')))
assert(NS.StreamState().status=='Completed')
assert(NS.ReceiveStream(frame(8,0,'reset')))
assert(NS.ReceiveStream(frame(9,0,'append','corrected')))
NS.FlushConversation(.05) assert(display=='corrected' and shown==0)
assert(not NS.ReceiveStream('ncs1\tr\tt\ts\t10\t9\tappend\n\255'))
assert(NS.ReceiveStream(frame(10,9,'error')))
assert(NS.StreamState().status=='Turn failed or was cancelled')
NS.Reply() assert(shown==1) -- Only explicit open shows the window.
print('PASS: partial before completion, UTF8, duplicates/conflicts, gap rejection, canonical reset, no focus/show side effects')

local function fresh()
 local ns={Notify=function() end}
 assert(loadfile('addon/Nanocodex/Bridge.lua'))('Nanocodex',ns)
 return ns
end
local function event(rid,sid,rev,offset,op,text)
 return 'ncs1\t'..rid..'\tt\t'..sid..'\t'..rev..'\t'..offset..'\t'..op..'\n'..(text or '')
end
local function finish(ns,rid,text)
 assert(ns.ReceiveStream(event(rid,'s',1,0,'append',text)))
 assert(ns.ReceiveStream(event(rid,'s',2,#text,'end')))
 assert(ns.ReceiveStream(event(rid,'s',3,#text,'done')))
end
local sustained=fresh()
assert(sustained.ReceiveStream(event('active','s',1,0,'append','unfinished')))
-- More than a lifetime's former block cap; preserve an interleaved active turn.
for i=1,160 do finish(sustained,'r'..i,'reply'..i) end
assert(sustained.StreamState().blocks==32)
assert(sustained.ReceiveStream(event('active','s',2,10,'append',' still here')))
assert(sustained.StreamState().text=='unfinished still here')
local before=sustained.StreamState()
assert(not sustained.ReceiveStream(event('r159','s',1,0,'append','conflict')))
assert(sustained.ReceiveStream(event('r160','s',3,8,'done'))) -- Retained exact replay.
assert(not sustained.ReceiveStream(event('r100','s',1,0,'append','reply100'))) -- Retired replay.
assert(sustained.StreamState().text==before.text and sustained.StreamState().bytes==before.bytes)

local bytes=fresh()
local chunk=string.rep('x',4000)
-- Each request fits, but their combined text exceeds the global display budget.
for request=1,3 do
 local rid='large'..request
 for i=1,40 do assert(bytes.ReceiveStream(event(rid,'s',i,(i-1)*4000,'append',chunk))) end
 assert(bytes.ReceiveStream(event(rid,'s',41,160000,'end')))
 assert(bytes.ReceiveStream(event(rid,'s',42,160000,'done')))
 assert(bytes.StreamState().bytes==160000)
 assert(bytes.StreamState().text==string.rep('x',160000))
end

local blocked=fresh()
for i=1,32 do
 assert(blocked.ReceiveStream(event('waiting'..i,'s',1,0,'append','a')))
 assert(blocked.ReceiveStream(event('waiting'..i,'s',2,1,'end')))
end
-- Sealed is not completed: preserve every block still awaiting turn done.
assert(not blocked.ReceiveStream(event('new','s',1,0,'append','b')))
assert(blocked.StreamState().blocks==32 and blocked.StreamState().bytes==32)
assert(blocked.ReceiveStream(event('waiting1','s',3,1,'done')))
assert(blocked.ReceiveStream(event('new','s',1,0,'append','b')))
assert(blocked.StreamState().blocks==32 and blocked.StreamState().bytes==32)

local failed=fresh()
for i=1,32 do
 assert(failed.ReceiveStream(event('failed'..i,'s',1,0,'append','a')))
 assert(failed.ReceiveStream(event('failed'..i,'s',2,1,'error')))
end
assert(failed.ReceiveStream(event('next','s',1,0,'append','b')))
-- Canonical reset reopens a completed request and protects it from eviction.
assert(failed.ReceiveStream(event('failed2','s',3,0,'reset')))
for i=1,40 do finish(failed,'later'..i,'z') end
assert(failed.ReceiveStream(event('failed2','s',4,0,'append','reopened')))
assert(failed.StreamState().text=='reopened')
print('PASS: bounded completed retention, byte pressure, active/ended/reset protection, failed-turn eviction and replay rejection')

local atomic=fresh()
finish(atomic,'completed',string.rep('c',1000))
for i=1,65 do assert(atomic.ReceiveStream(event('growing','s',i,(i-1)*4000,'append',chunk))) end
local snapshot=atomic.StreamState()
assert(not atomic.ReceiveStream(event('growing','s',66,260000,'append',chunk)))
-- Insufficient reclaimable space must not partially evict completed history.
assert(atomic.StreamState().blocks==snapshot.blocks and atomic.StreamState().bytes==snapshot.bytes)
assert(atomic.ReceiveStream(event('completed','s',3,1000,'done')))
local multi=fresh()
assert(multi.ReceiveStream(event('multi','one',1,0,'append','a')))
assert(multi.ReceiveStream(event('multi','two',1,0,'append','b')))
assert(multi.ReceiveStream(event('multi','one',2,1,'error')))
for i=1,40 do finish(multi,'other'..i,'x') end
-- One failed block must not make another still-active block evictable.
assert(multi.ReceiveStream(event('multi','two',2,1,'append','c')))
assert(multi.StreamState().text=='a\n\nbc')
print('PASS: failed admission is atomic and partially failed multi-block requests remain intact')
