-- Exercise real receiver/client routing with 192 commentary blocks and 192 tools.
local NS={Notify=function()end}
local display=''
NS.DisplayConversation=function(t)display=t end
NS.Encode=function()return 'q' end
NS.TransportSend=function()return false,'pending' end
NS.TransportStatus=function()return {connected=true,pending=false}end
for _,file in ipairs({'Projects','Bridge','Client'}) do assert(loadfile('addon/Nanocodex/'..file..'.lua'))('Nanocodex',NS) end
local function receive(kind,value) assert(NS.OnTransportMessage(kind,value),kind..' rejected') NS.FlushConversation(.05) end
local function stream(r,s,rev,off,op,body)
 local v='ncs1\t'..r..'\tturn-'..r..'\t'..s..'\t'..rev..'\t'..off..'\t'..op..'\n'..(body or '')
 receive('stream',v) receive('stream',v) -- Every envelope is replayed exactly once.
end
receive('projects','ncw1\nP\tp\tStress\nT\tp\ta\tAlpha\tidle\nT\tp\tb\tBeta\tidle')
for _,r in ipairs({'a','b'})do
 assert(NS.LoadThread('p',r))
 receive('reply','nch1\th'..r..'\t'..r..'\t0\t1\t\t0\t'..NS.ClientState().view..'\nHistory '..r)
 receive('ack','ncm1\tsend\t'..r..'\t'..r..'\tturn-'..r..'\tremoteaccepted')
end
for i=1,96 do
 for _,r in ipairs({'a','b'}) do
  local text=r..' step '..i..' Ω'
  stream(r,'c'..i,1,0,'append',text)
  receive('ack','ncm1\ttool\t'..r..'\tturn-'..r..'\texec_command\trunning')
  if i%3==0 then assert(NS.LoadThread('p',r)) NS.FlushConversation(.05) end
  receive('ack','ncm1\ttool\t'..r..'\tturn-'..r..'\texec_command\tcompleted')
  stream(r,'c'..i,2,#text,'end')
 end
end
assert(NS.StreamState().blocks==192)
assert(NS.LoadThread('p','a')) NS.FlushConversation(.05)
assert(display:find('a step 96',1,true) and not display:find('b step',1,true))
assert(NS.ClientActivity():find('exec_command · completed',1,true))
stream('b','final',1,0,'append','B FINAL') stream('b','final',2,7,'end') stream('b','final',3,7,'done')
assert(NS.ClientThreadStatus('b')==nil and NS.ClientThreadStatus('a')=='Working')
assert(not display:find('B FINAL',1,true))
receive('ack','ncm1\ttool\tb\tturn-b\texec_command\trunning')
assert(NS.ClientThreadStatus('b')==nil,'stale activity cannot resurrect a completed turn')
stream('a','final',1,0,'append','A FINAL') stream('a','final',2,7,'end') stream('a','final',3,7,'done')
assert(display:find('A FINAL',1,true) and NS.ClientActivity()==nil)
assert(NS.StreamState().receipt_bytes<=512*1024)
-- Cancellation is terminal only when a stream error arrives, never on its receipt.
receive('ack','ncm1\tsend\tcancel\ta\tturn-cancel\tremoteaccepted')
assert(NS.StopThread()) receive('ack','ncm1\tstop\ta\tturn-cancel\trequested')
assert(NS.ClientThreadStatus('a')=='Working')
stream('cancel','s',1,0,'append','partial') stream('cancel','s',2,7,'error')
assert(NS.ClientThreadStatus('a')==nil and display:find('partial',1,true))
NS.TransportStatus=function()return {connected=true,pending=false,inflight=0}end
assert(NS.TransportDisplay():find('Turn failed or was cancelled',1,true),'idle transport must preserve terminal status')
print('PASS: 192 tools across concurrent chats, 192 commentary blocks, replay, switching, terminal status and cancellation')
-- Canonical corrections must not make replay receipts grow without bound.
local ns={Notify=function()end};assert(loadfile('addon/Nanocodex/Bridge.lua'))('Nanocodex',ns)
local body=string.rep('x',3900)
for i=1,600 do
 local rev=(i-1)*2+1
 assert(ns.ReceiveStream('ncs1\tr\tt\ts\t'..rev..'\t0\treset\n'))
 assert(ns.ReceiveStream('ncs1\tr\tt\ts\t'..(rev+1)..'\t0\tappend\n'..body))
end
assert(ns.StreamState().bytes==3900 and ns.StreamState().receipt_bytes<=512*1024)
print('PASS: 600 canonical replacements retain bounded text and replay receipts')
