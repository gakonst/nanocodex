local _, NS = ...
-- UI contract: TransportSend accepts an encoded request. A true return means
-- transport acknowledged, never proof of backend acceptance. false/pending means queued.
-- No credentials belong here.
-- Confirmed Transport.lua contract:
--   TransportStatus(): {connected=boolean, state=string, pending=boolean,
--     acknowledged=boolean, message=string, bytes_acked=number, bytes_total=number, ...}
--   OnTransportMessage("reply", plainText), ("projects", ncw1Snapshot),
--   ("transport_ack", carrierReceipt), ("ack", backendReceipt), ("error", plainText).
-- Unknown/missing status stays disconnected; an ack never implies model success.
local lastRequest = "No requests queued"
local function safe(value)
    return tostring(value or ""):gsub("[%z\1-\8\11\12\14-\31\127]", ""):gsub("|", "||")
end
function NS.TransportDisplay()
    if type(NS.TransportStatus) ~= "function" then
        return "Disconnected · transport unavailable\n" .. lastRequest
    end
    local ok, status = pcall(NS.TransportStatus)
    if not ok then return "Disconnected · transport status unavailable\n" .. lastRequest end
    -- A recent peer frame proves only the transport link, not backend/model availability.
    local connected = type(status) == "table" and status.connected == true
    local detail = type(status) == "table" and (status.message or status.state) or status
    return (connected and "Bridge linked" or "Disconnected") ..
        (type(detail) == "string" and " · " .. safe(detail) or "") .. "\n" .. (NS.ClientStatus and NS.ClientStatus() .. " · " or "") .. lastRequest
end
function NS.QueueRequest(request)
    if type(NS.TransportSend) ~= "function" then
        lastRequest = "Not queued · transport unavailable"
        NS.Notify("error", "Disconnected: transport unavailable. Request was not queued.")
        return false, "transport unavailable"
    end
    local encodedOK, payload = pcall(NS.Encode, request)
    if not encodedOK then
        lastRequest = "Not queued · encoding failed"
        NS.Notify("error", "Request could not be encoded.")
        return false, "encoding failed"
    end
    local ok, acknowledged, err = pcall(NS.TransportSend, payload, true)
    if ok and acknowledged == false and err == "pending" then
        lastRequest = "Queued · awaiting transport acknowledgement"
        NS.Notify("status", lastRequest)
        return true, "pending"
    end
    if ok and acknowledged == false and err == "busy" then
        -- Preserve the outstanding request's state; this new action was not queued.
        NS.Notify("error", "Not queued · another request is awaiting transport acknowledgement.")
        return false, "busy"
    end
    if not ok or acknowledged ~= true then
        lastRequest = "Not queued · " .. safe(ok and (err or "transport rejected request") or "transport error")
        NS.Notify("error", lastRequest)
        return false, ok and err or "transport error"
    end
    lastRequest = "Transport acknowledged · awaiting result"
    NS.Notify("status", lastRequest)
    return true
end
function NS.Ask(prompt, gameMode)
    prompt = type(prompt) == "string" and prompt:match("^%s*(.-)%s*$") or ""
    if prompt == "" then NS.Notify("error", "Type your question in the Nanocodex panel or use /nc ask <question>.") return false end
    local projectID, threadID
    if not gameMode and NS.ProjectSelection then projectID, threadID = NS.ProjectSelection() end
    if projectID and not threadID then NS.Notify("error", "Use /nc projects to select a chat first.") return false end
    if not NS.Snapshot() then return false end
    local accepted, reason = NS.QueueRequest({
        schemaVersion = 1, source = "nanocodex-wow", type = "nanocodex.ask",
        mode = projectID and "agent" or "hint", project_id = projectID, thread_id = threadID,
        prompt = prompt, context = NanocodexWowDB.lastContext,
    })
    if accepted then
        if (gameMode or not projectID) and NS.ClientGameSent then NS.ClientGameSent()
        elseif NS.ClientSent then NS.ClientSent(prompt) end
    end
    return accepted, reason
end

local MAX_ANSWER = 256 * 1024
local lastReply, displayDirty, displayElapsed = "Waiting for a reply.", false, 0
-- Raw bytes are kept independently from markup-escaped display text.
local function setReply(value)
    lastReply = value
    displayDirty = true
end
function NS.FlushConversation(elapsed)
    displayElapsed = displayElapsed + (elapsed or 0)
    if displayElapsed < 0.05 then return end
    displayElapsed = 0
    if displayDirty and NS.DisplayConversation then
        displayDirty = false
        NS.DisplayConversation(safe(lastReply))
    end
end
function NS.Reply(value)
    if type(value) == "string" then
        if #value > MAX_ANSWER then
            NS.Notify("error", "Reply exceeds the 256 KiB display limit; request a shorter answer or next page.")
            return false
        end
        setReply(value)
    else
        if NS.OpenConversation then NS.OpenConversation() end
    end
    return true
end
-- ncs1 uses UTF-8 byte offsets, stable block identity and monotonically increasing
-- revisions. Never execute payloads or infer backend success from carrier ACKs.
local streams, order, activeRequest, retainedBytes = {}, {}, nil, 0
local completed, retired, retiredOrder = {}, {}, {}
-- Retain recent completed replies until space is needed. Never evict a request
-- with an unfinished block, including an ended block still awaiting turn done.
-- Recent eviction tombstones reject late application replays without retaining
-- reply text. NC1 also rejects old application message IDs within its session.
local function makeRoom(extraBlocks, extraBytes, protected)
    if #order+extraBlocks<=32 and retainedBytes+extraBytes<=MAX_ANSWER then return true end
    local candidates, seen = {}, {}
    for _,key in ipairs(order) do
        local group=streams[key].group
        if group~=protected and completed[group] and not seen[group] then
            seen[group]=true
            local candidate={group=group,keys={},bytes=0,eligible=true}
            for _,other in ipairs(order) do
                local entry=streams[other]
                if entry.group==group then
                    candidate.keys[#candidate.keys+1]=other
                    candidate.bytes=candidate.bytes+#entry.text
                    if not entry.sealed then candidate.eligible=false end
                end
            end
            if candidate.eligible then candidates[#candidates+1]=candidate end
        end
    end
    local blocks,bytes,count=#order+extraBlocks,retainedBytes+extraBytes,0
    for _,candidate in ipairs(candidates) do
        count=count+1 blocks=blocks-#candidate.keys bytes=bytes-candidate.bytes
        if blocks<=32 and bytes<=MAX_ANSWER then break end
    end
    if blocks>32 or bytes>MAX_ANSWER then return false end
    for i=1,count do
        local candidate=candidates[i]
        for _,key in ipairs(candidate.keys) do streams[key]=nil end
        retainedBytes=retainedBytes-candidate.bytes
        completed[candidate.group]=nil
        retired[candidate.group]=true retiredOrder[#retiredOrder+1]=candidate.group
        if #retiredOrder>128 then retired[table.remove(retiredOrder,1)]=nil end
    end
    local kept={}
    for _,key in ipairs(order) do if streams[key] then kept[#kept+1]=key end end
    order=kept
    return true
end
local function validUTF8(value)
    local i=1
    while i<=#value do
        local a=value:byte(i)
        local count,low,high=0,128,191
        if a<128 then count=1
        elseif a>=194 and a<=223 then count=2
        elseif a>=224 and a<=239 then count=3 if a==224 then low=160 elseif a==237 then high=159 end
        elseif a>=240 and a<=244 then count=4 if a==240 then low=144 elseif a==244 then high=143 end
        else return false end
        if i+count-1>#value then return false end
        for j=1,count-1 do
            local b=value:byte(i+j)
            if b<(j==1 and low or 128) or b>(j==1 and high or 191) then return false end
        end
        i=i+count
    end
    return true
end
local function identity(value)
    return value and #value>=1 and #value<=128 and not value:find('[^%w%._:%-]')
end
local function displayStreams()
    local texts={}
    for _,key in ipairs(order) do
        local stream=streams[key]
        if stream.request==activeRequest and stream.text~='' then texts[#texts+1]=stream.text end
    end
    setReply(table.concat(texts,'\n\n'))
end
function NS.ReceiveStream(value)
    if type(value)~='string' or #value>4096 then return false end
    local header,body=value:match('^([^\n]*)\n(.*)$')
    if not header or not validUTF8(body) then return false end
    local version,rid,turn,sid,rev,offset,op=header:match('^([^\t]+)\t([^\t]+)\t([^\t]+)\t([^\t]+)\t(%d+)\t(%d+)\t([^\t]+)$')
    if version~='ncs1' or not identity(rid) or not identity(turn) or not identity(sid) then return false end
    rev,offset=tonumber(rev),tonumber(offset)
    if not rev or rev<1 or rev>1000000000 or not offset or offset>MAX_ANSWER then return false end
    if op~='append' and op~='reset' and op~='end' and op~='done' and op~='error' then return false end
    if op~='append' and body~='' then return false end
    local group=rid..'/'..turn
    if retired[group] then return false end
    local key=group..'/'..sid
    local stream=streams[key]
    if stream and rev<=stream.revision then
        -- Exact replay only; a reused revision with different bytes is a conflict.
        return stream.receipts[rev]==value
    end
    if not stream then
        if (rev~=1 and op~='reset') or offset~=0 then return false end
        stream={request=rid,turn=turn,id=sid,group=group,revision=0,text='',receipts={}}
    end
    if op=='reset' then
        if offset~=0 then return false end
    elseif rev~=stream.revision+1 or offset~=#stream.text then
        return false
    end
    if stream.sealed and op~='reset' and op~='done' then return false end
    if op=='done' and not stream.sealed then return false end
    local extraBytes=op=='append' and #body or op=='reset' and -#stream.text or 0
    if not makeRoom(streams[key] and 0 or 1,extraBytes,group) then
        lastRequest='Answer exceeded the display limit; open it in Nanocodex.'
        return false
    end
    if not streams[key] then streams[key]=stream order[#order+1]=key end
    if op=='reset' then
        retainedBytes=retainedBytes-#stream.text stream.text='' stream.sealed=false
        completed[group]=nil
    elseif op=='append' then
        stream.text=stream.text..body retainedBytes=retainedBytes+#body
    elseif op=='end' or op=='error' then stream.sealed=true end
    if op=='done' or op=='error' then completed[group]=true end
    stream.revision=rev stream.receipts[rev]=value
    stream.receipts[rev-256]=nil
    activeRequest=rid
    lastRequest=op=='done' and 'Completed' or op=='error' and 'Turn failed or was cancelled' or 'Streaming reply…'
    local texts={}
    for _,streamKey in ipairs(order) do
        local entry=streams[streamKey]
        if entry.request==rid and entry.text~='' then texts[#texts+1]=entry.text end
    end
    if not NS.ClientStream or not NS.ClientStream(rid,turn,table.concat(texts,'\n\n'),lastRequest) then displayStreams() end
    return true
end
function NS.StreamState()
    return {request=activeRequest,bytes=retainedBytes,blocks=#order,text=lastReply,status=lastRequest}
end

function NS.OnTransportMessage(kind, value)
    if type(kind) ~= "string" or type(value) ~= "string" then return false end
    if NS.ReceiveClientMessage then
        local handled=NS.ReceiveClientMessage(kind,value)
        if handled~=nil then
            if handled and kind=="reply" and value:sub(1,5)~="nch1\t" then lastRequest="Reply received" end
            return handled
        end
    end
    if kind == "stream" then
        return NS.ReceiveStream(value)
    elseif kind == "reply" then
        if not NS.Reply(value) then return false end
        lastRequest = "Reply received"
    elseif kind == "projects" then
        if not NS.ImportProjects or not NS.ImportProjects(value) then return false end
        lastRequest = "Project snapshot received"
    elseif kind == "transport_ack" then
        lastRequest = "Transport acknowledged · awaiting backend acceptance"
    elseif kind == "ack" then
        -- Backend receipts are a distinct application message. Display their
        -- bounded plain text so queueing, admission and action completion differ.
        lastRequest = safe(value)
    elseif kind == "error" then
        lastRequest = "Request failed · " .. safe(value)
        NS.Notify("error", lastRequest)
    else
        return false
    end
    return true
end
