-- Consumes real Python-generated NC1 frame hex on stdin; no game APIs/input.
local NS={Notify=function() end,DisplayConversation=function() end}
assert(loadfile('addon/Nanocodex/Bridge.lua'))('Nanocodex',NS)
assert(loadfile('addon/Transport.lua'))('Nanocodex',NS)
local T=NS.Transport
local link=T.New(19)
local seenPartial=false
T.NewApplication(link,function() return 1 end,function(kind,text)
    assert(kind=='stream')
    local accepted=NS.OnTransportMessage(kind,text)
    local state=NS.StreamState()
    if state.text=='Hello ' then assert(state.status=='Streaming reply…') seenPartial=true end
    return accepted
end)
for hex in io.lines() do
    local packet=hex:gsub('..',function(pair) return string.char(tonumber(pair,16)) end)
    assert(link:Receive(packet))
    assert(link:Receive(packet)) -- Lost ACK causes replay; no double append.
end
assert(seenPartial,'No visible partial result before completion')
assert(NS.StreamState().text=='Hello \206\169!')
assert(NS.StreamState().status=='Completed')
print('PASS: Python WebSocket-event projection → NC1 framing/CRC → Lua assembly → incremental addon receiver')
