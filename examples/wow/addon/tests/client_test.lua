-- State contract uses real Projects, Bridge and Client code, without game APIs.
local NS={Notify=function() end}
local display, sent, busy='', {}, false
NS.DisplayConversation=function(text) display=text end
NS.Encode=function(request) sent[#sent+1]=request return 'encoded' end
NS.TransportSend=function() return false,busy and 'busy' or 'pending' end
NS.TransportStatus=function() return {pending=busy,connected=true} end
for _,file in ipairs({'Projects','Bridge','Client'}) do assert(loadfile('addon/Nanocodex/'..file..'.lua'))('Nanocodex',NS) end
local function receive(kind,text) assert(NS.OnTransportMessage(kind,text)) NS.FlushConversation(.05) end
local function history(snapshot,thread,page,pages,before,more,view,body)
 return 'nch1\t'..snapshot..'\t'..thread..'\t'..page..'\t'..pages..'\t'..before..'\t'..more..'\t'..view..'\n'..body
end
receive('projects','ncw1\nP\tp\tProject\nT\tp\ta\tAlpha\tunknown\nT\tp\tb\tBeta\tclosed')
assert(#NS.ThreadEntries()==2 and #NS.ThreadEntries('beta')==1)
assert(NS.LoadThread('p','a'))
assert(sent[#sent].action=='load_history' and sent[#sent].view_id=='1')
receive('reply',history('h1','a',0,2,'50','1','1','You:\nQuestion\n\nAssistant:\n'))
assert(NS.ClientState().history=='Loading conversation…', 'partial pages must not replace a conversation')
NS.PumpClient()
assert(sent[#sent].snapshot_id=='h1' and sent[#sent].page==1)
receive('reply',history('h1','a',1,2,'50','1','1','Answer |Ω'))
assert(display=='You:\nQuestion\n\nAssistant:\nAnswer ||Ω')
assert(NS.LoadEarlier()) assert(sent[#sent].before=='50' and sent[#sent].view_id=='2')
receive('reply',history('h2','a',0,1,'10','0','2','You:\nOlder'))
assert(display:find('Older',1,true) and display:find('Answer',1,true))
assert(not NS.LoadEarlier())
-- Busy selection leaves both the selected thread and visible conversation intact.
busy=true assert(not NS.LoadThread('p','b'))
local _,thread=NS.ProjectSelection() assert(thread=='a')
busy=false assert(NS.LoadThread('p','b'))
receive('reply',history('late','a',0,1,'','0','2','STALE'))
assert(not display:find('STALE',1,true))
receive('reply',history('b1','b',0,1,'','0','3','Beta history'))
assert(display=='Beta history')
receive('ack','ncm1\treply\tlegacy\ta\tlegacy-turn')
receive('reply','Background legacy reply')
assert(display=='Beta history', 'legacy backend completion must preserve selected thread')
-- Send identities route incoming stream updates; background completion cannot steal view.
receive('ack','ncm1\tsend\tr1\ta\tt1\tremoteaccepted')
receive('stream','ncs1\tr1\tt1\ts\t1\t0\tappend\nBackground')
assert(display=='Beta history')
receive('ack','ncm1\tsend\tr2\tb\tt2\tlocal_queued')
receive('stream','ncs1\tr2\tt2\ts\t1\t0\tappend\nVisible |reply')
assert(display:find('Visible ||reply',1,true) and display:find('Beta history',1,true))
assert(NS.StopThread()) assert(sent[#sent].thread_id=='b' and sent[#sent].turn_id=='t2')
receive('ack','ncm1\tstop\tb\tt2\trequested')
assert(NS.StopThread(), 'request receipt alone does not prove turn stopped')
receive('stream','ncs1\tr2\tt2\ts\t2\t14\tend\n')
receive('stream','ncs1\tr2\tt2\ts\t3\t14\tdone\n')
assert(not NS.StopThread())
receive('ack','ncm1\tconnection\tconnected') assert(NS.TransportDisplay():find('Account connected',1,true))
assert(NS.ReconnectClient() and sent[#sent].action=='reconnect')
-- Roster pages are validated only after atomic assembly, retain old roster until complete.
receive('ack','ncm1\tprojects\troster1\t0\t2')
receive('projects','ncw1\nP\tnew\tNew')
assert(#NS.ThreadEntries()==2)
NS.PumpClient() assert(sent[#sent].snapshot_id=='roster1')
receive('ack','ncm1\tprojects\troster1\t1\t2')
receive('projects','ncw1\nT\tnew\tnew-thread\tCreated\tunknown')
assert(#NS.ThreadEntries()==1 and NS.ThreadEntries()[1].id=='new-thread')
assert(NS.OnTransportMessage('ack','ncm1\tprojects\tbad\t-1\t2')==false)
assert(NS.OnTransportMessage('reply',history('bad','b',0,33,'','0','3','oversized page count'))==false)
print('PASS: native roster paging, history paging/earlier/stale selection, send identity routing, stop receipts and connection status')

-- Sending while a history page is in flight fences late pages and continuations.
assert(NS.LoadThread('new','new-thread'))
local pendingView = NS.ClientState().view
receive('reply',history('inflight','new-thread',0,2,'','0',pendingView,'Old history'))
NS.ClientSent('New question')
NS.FlushConversation(.05)
local visible = display
local sentBefore = #sent
NS.PumpClient()
assert(#sent == sentBefore, 'superseded history continuation must be discarded')
receive('reply',history('inflight','new-thread',1,2,'','0',pendingView,'LATE HISTORY'))
assert(display == visible and display:find('New question',1,true))
print('PASS: send fences history already in flight and discards its queued continuation')

-- Large independent-project accounts exceed the old 1,000 combined-row limit.
local roster={"ncw1"}
for i=1,600 do
    roster[#roster+1]="P\tp"..i.."\tProject "..i
    roster[#roster+1]="T\tp"..i.."\tt"..i.."\tChat "..i.."\tidle"
end
receive('projects',table.concat(roster,"\n"))
assert(#NS.ThreadEntries()==600)
-- Roster page count is independent of the smaller conversation retention cap.
receive('ack','ncm1\tprojects\tlarge\t0\t128')
assert(NS.OnTransportMessage('ack','ncm1\tprojects\ttoo-large\t0\t129')==false)
print('PASS: 600 independent projects and 128-page roster envelope capacity')
-- Assemble a catalog larger than the former byte/page ceilings through real
-- client envelopes, while preserving the prior complete catalog until the end.
local catalogBytes=0
for page=0,39 do
    local part={'ncw1'}
    for offset=1,15 do
        local i=page*15+offset
        part[#part+1]='P\tlarge-p'..i..'\t'..string.rep('Project',70)
        part[#part+1]='T\tlarge-p'..i..'\tlarge-t'..i..'\t'..string.rep('Chat',70)..'\tidle'
    end
    part=table.concat(part,'\n') catalogBytes=catalogBytes+#part
    receive('ack','ncm1\tprojects\tlarge-complete\t'..page..'\t40')
    receive('projects',part)
    if page<39 then
        assert(NS.ThreadEntries()[1].id=='t1','partial catalog must stay invisible')
        NS.PumpClient()
        assert(sent[#sent].snapshot_id=='large-complete' and sent[#sent].page==page+1)
    end
end
assert(catalogBytes>256*1024 and catalogBytes<1024*1024)
assert(#NS.ThreadEntries()==600 and NS.ThreadEntries()[600].id=='large-t600')
print('PASS: 40-page catalog over 256 KiB assembles atomically with all 600 chats')
