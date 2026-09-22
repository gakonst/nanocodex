local _, NS = ...
-- Native client state. All account I/O stays in the companion; these are bounded
-- application messages carried by the existing NC1 transport.
local MAX = 256 * 1024
local MAX_ROSTER, MAX_ROSTER_PAGES = 1024 * 1024, 128
local client = {view=0, account="Account unchecked", history="", live="", turns={}, requests={}, requestOrder={}, reads={}}
local function selection() if NS.ProjectSelection then return NS.ProjectSelection() end end
local function decode(value)
    if value:gsub("%%[%x][%x]", ""):find("%%") then return nil end
    value = value:gsub("%%(%x%x)", function(h) return string.char(tonumber(h,16)) end)
    if value:find("[%z\1-\31\127]") then return nil end
    return value
end
local function fields(value)
    local result={}
    for field in (value .. "\t"):gmatch("([^\t]*)\t") do
        local decoded=decode(field) if not decoded then return nil end
        result[#result+1]=decoded
    end
    return result
end
local function action(name, values)
    local request={schemaVersion=1,source="nanocodex-wow",type="nanocodex.action",action=name}
    for key,value in pairs(values or {}) do request[key]=value end
    return request
end
local function defer(request)
    if #client.reads>=4 then NS.Notify("error", "Refresh queue full; use Refresh again after delivery.") return false end
    client.reads[#client.reads+1]=request
    return true
end
function NS.PumpClient()
    if #client.reads==0 or not NS.TransportStatus then return end
    local ok,status=pcall(NS.TransportStatus)
    if not ok or type(status)~="table" or status.pending then return end
    local request=table.remove(client.reads,1)
    -- Only read-only continuation requests use this queue. Failed sends are never replayed.
    NS.QueueRequest(request)
end
local function discardHistoryReads()
    for i=#client.reads,1,-1 do
        if client.reads[i].action=="load_history" then table.remove(client.reads,i) end
    end
end
local function render()
    local text=client.history
    if client.live~="" then text=text..(text~="" and "\n\nAssistant\n" or "")..client.live end
    if #text>MAX then text="Conversation exceeds the in-game display limit. Open it in the desktop companion." end
    NS.Reply(text~="" and text or "No messages yet.")
end
function NS.ClearThreadView()
    discardHistoryReads()
    client.view=client.view+1 client.thread=nil client.game=false
    client.history,client.live,client.historyPage,client.prepend="Select a chat to load its history.","",nil,nil
    client.before,client.hasMore,client.expectedGame=nil,false,nil
    render()
end
function NS.ClientStatus() return client.account end
function NS.ClientState() return {thread=client.thread,view=tostring(client.view),before=client.before,has_more=client.hasMore,history=client.history,account=client.account} end
function NS.RefreshWorkspace()
    local accepted=NS.QueueRequest(action("refresh_projects", {page=0}))
    if accepted then defer(action("connection_status")) end
    return accepted
end
function NS.LoadThread(projectID, threadID)
    local _,thread=selection()
    thread=threadID or thread
    if not thread then NS.Notify("error", "Select a chat first.") return false end
    local view=tostring(client.view+1)
    if not NS.QueueRequest(action("load_history",{thread_id=thread,view_id=view})) then return false end
    discardHistoryReads()
    if projectID then NS.SelectProject(projectID, thread) end
    client.view,client.thread=client.view+1,thread
    client.game,client.prepend=false,nil
    client.history,client.live,client.before,client.hasMore,client.historyPage="Loading conversation…","",nil,false,nil
    render()
    return true
end
function NS.LoadEarlier()
    if not client.thread or not client.hasMore or not client.before then NS.Notify("status", "No earlier messages available. Refresh the thread to check again.") return false end
    if client.historyPage then NS.Notify("status", "Wait for the current history page.") return false end
    local view=tostring(client.view+1)
    if not NS.QueueRequest(action("load_history",{thread_id=client.thread,view_id=view,before=client.before})) then return false end
    client.view=client.view+1 client.prepend=true
    return true
end
function NS.ReconnectClient()
    -- This resumes companion stream subscriptions and checks account access. It
    -- cannot restart a lost pixel/keyboard session or perform account sign-in.
    return NS.QueueRequest(action("reconnect"))
end
function NS.StopThread()
    local _,thread=selection()
    if client.game then thread=client.thread end
    local turn=thread and client.turns[thread]
    if not turn then NS.Notify("status", "No active turn known for this chat. Refresh in the companion for turns started elsewhere.") return false end
    return NS.QueueRequest(action("stop_turn",{thread_id=thread,turn_id=turn}))
end
function NS.ClientGameSent()
    discardHistoryReads()
    client.view=client.view+1 client.game=true client.thread=nil
    client.history,client.live,client.historyPage,client.prepend="","",nil,nil
    client.before,client.hasMore,client.expectedGame=nil,false,nil
    local ok,status=pcall(NS.TransportStatus)
    if ok and type(status)=="table" and type(status.session)=="number" and type(status.message_id)=="number" then
        client.expectedGame=string.format("ncw:%08x:%04x",status.session,status.message_id)
    end
    render()
end
function NS.ClientSent(prompt)
    local _,thread=selection()
    if thread and thread==client.thread then
        -- A send supersedes any pending history view, including queued pages.
        -- Late history must not erase the optimistic prompt or streamed answer.
        discardHistoryReads()
        client.view=client.view+1 client.historyPage,client.prepend=nil,nil
        local prior=client.history..(client.live~="" and "\n\nAssistant\n"..client.live or "")
        client.history=prior.."\n\nYou\n"..prompt client.live=""
        render()
    end
end
function NS.ClientStream(rid, turn, text, status)
    local thread=client.requests[rid]
    if not thread then return false end -- Legacy/manual streams keep their display path.
    if status=="Completed" or status=="Turn failed or was cancelled" then
        if client.turns[thread]==turn then client.turns[thread]=nil end
    end
    if not client.thread then return client.view>0 end
    local _,selected=selection()
    if (selected==thread or client.game) and client.thread==thread then client.live=text render() end
    return true -- A background thread must never replace the selected conversation.
end
local function remember(rid,thread)
    if not client.requests[rid] then client.requestOrder[#client.requestOrder+1]=rid end
    client.requests[rid]=thread
    if #client.requestOrder>128 then client.requests[table.remove(client.requestOrder,1)]=nil end
end
function NS.ReceiveClientMessage(kind,value)
    -- A correlated plain response is data even when its text resembles a header.
    if kind=="reply" and client.reply then
        local receipt=client.reply client.reply=nil
        if not NS.ClientStream(receipt.rid,receipt.turn,value,"Completed") then return NS.Reply(value) end
        return true
    end
    if kind=="ack" and value:sub(1,5)=="ncm1\t" then
        local f=fields(value) if not f then return false end
        if f[2]=="send" and #f==6 and (f[6]=="local_queued" or f[6]=="remoteaccepted") then
            remember(f[3],f[4]) client.turns[f[4]]=f[5]
            if client.game and not client.thread and client.expectedGame==f[3] then client.thread=f[4] end
            return true
        elseif f[2]=="reply" and #f==5 then
            client.reply={rid=f[3],thread=f[4],turn=f[5]} remember(f[3],f[4]) return true
        elseif f[2]=="connection" and #f==3 and (f[3]=="connected" or f[3]=="disconnected") then
            client.account=f[3]=="connected" and "Account connected" or "Account disconnected · sign in through companion"
            return true
        elseif f[2]=="stop" and #f==5 and f[5]=="requested" then
            NS.Notify("status","Stop requested; waiting for turn completion.") return true
        elseif f[2]=="created" and #f==6 then
            client.created={project=f[4],thread=f[5]}
            defer(action("refresh_projects",{page=0})) return true
        elseif f[2]=="projects" and #f==5 then
            local page,pages=tonumber(f[4]),tonumber(f[5])
            if not page or not pages or page%1~=0 or pages%1~=0 or page<0 or page>=pages or pages>MAX_ROSTER_PAGES then return false end
            if page==0 then client.roster={id=f[3],pages=pages,next=0,parts={},bytes=4} end
            local r=client.roster
            if not r or r.id~=f[3] or r.next~=page or r.pages~=pages then return false end
            client.projectHeader=true return true
        end
        return false
    elseif kind=="projects" and client.projectHeader then
        local r=client.roster
        if not r or value:sub(1,4)~="ncw1" or (#value>4 and value:sub(5,5)~="\n") then return false end
        local body=value:sub(5)
        if r.bytes+#body>MAX_ROSTER then return false end
        if r.next+1==r.pages then
            local complete="ncw1"..table.concat(r.parts)..body
            if not NS.ImportProjects(complete) then return false end
            client.roster=nil
            if client.created then
                local made=client.created client.created=nil
                if NS.SelectProject(made.project,made.thread) then
                    local view=tostring(client.view+1)
                    client.view,client.thread=client.view+1,made.thread
                    client.before,client.hasMore,client.game,client.expectedGame=nil,false,false,nil
                    client.history,client.live,client.historyPage,client.prepend="","",nil,nil
                    render()
                    defer(action("load_history",{thread_id=made.thread,view_id=view}))
                end
            end
        else
            r.parts[#r.parts+1]=body r.bytes=r.bytes+#body r.next=r.next+1
            defer(action("refresh_projects",{snapshot_id=r.id,page=r.next}))
        end
        client.projectHeader=nil return true
    elseif kind=="reply" and value:sub(1,5)=="nch1\t" then
        local header,body=value:match("^([^\n]*)\n(.*)$")
        local f=header and fields(header)
        if not f or #f~=8 then return false end
        local page,pages=tonumber(f[4]),tonumber(f[5])
        if not page or not pages or page%1~=0 or pages%1~=0 or page<0 or page>=pages or pages>32 or (f[7]~="0" and f[7]~="1") then return false end
        if f[3]~=client.thread or f[8]~=tostring(client.view) then return true end -- Valid stale read: consume, never display.
        if page==0 then client.historyPage={id=f[2],pages=pages,next=0,parts={},bytes=0} end
        local h=client.historyPage
        if not h or h.id~=f[2] or h.next~=page or h.pages~=pages or h.bytes+#body>MAX then return false end
        if page+1==pages then
            local complete=table.concat(h.parts)..body
            if client.prepend then complete=complete.."\n\n"..client.history end
            if #complete>MAX then NS.Notify("error","History exceeds the in-game display limit; open it in the companion.")
            else client.history=complete end
            client.historyPage,client.prepend=nil,nil
            client.before=f[6]~="" and f[6] or nil client.hasMore=f[7]=="1"
            render()
        else
            h.parts[#h.parts+1]=body h.bytes=h.bytes+#body h.next=h.next+1
            defer(action("load_history",{thread_id=client.thread,view_id=tostring(client.view),snapshot_id=h.id,page=h.next}))
        end
        return true
    end
    return nil
end
