-- Stub lifecycle coverage; not a replacement for real WoW secure/UI testing.
-- Run from repo root: lua addon/tests/ui_test.lua
local frames = {}
local methods = {}
function methods:SetScript(event, callback) self.scripts[event] = callback end
function methods:RegisterEvent(event) self.events[event] = true end
function methods:UnregisterEvent(event) self.events[event] = nil end
function methods:SetPoint(...) self.point = {...} end
function methods:ClearAllPoints() self.point = nil end
function methods:GetPoint() return (table.unpack or unpack)(self.point or {"CENTER", UIParent, "CENTER", 0, 0}) end
function methods:Show() local was = self.visible self.visible = true if not was and self.scripts.OnShow then self.scripts.OnShow(self) end end
function methods:Hide() local was = self.visible self.visible = false if was and self.scripts.OnHide then self.scripts.OnHide(self) end end
function methods:IsShown() return self.visible end
function methods:SetText(value) self.value = value if self.scripts.OnTextChanged then self.scripts.OnTextChanged(self) end end
function methods:GetText() return self.value or "" end
function methods:SetFocus() self.focus = true end
function methods:ClearFocus() self.focus = false end
function methods:HighlightText() self.highlighted = true end
function methods:SetScrollChild(child) self.child = child end
local function newFrame(name)
    local f = setmetatable({ scripts={}, events={}, visible=true, name=name }, {__index=methods})
    frames[#frames+1] = f
    if name then _G[name] = f end
    return f
end
function methods:CreateFontString(_, _, font) local f = newFrame() f.font = font return f end
for _, name in ipairs({"SetSize", "SetFrameStrata", "SetClampedToScreen", "EnableMouse", "SetMovable", "RegisterForDrag", "StartMoving", "StopMovingOrSizing", "SetBackdrop", "SetBackdropColor", "SetBackdropBorderColor", "SetWidth", "SetHeight", "SetJustifyH", "SetMultiLine", "SetAutoFocus", "SetFontObject", "SetMaxLetters", "UpdateScrollChildRect"}) do methods[name] = function() end end
function methods:SetBackdrop(value) self.backdrop = value end
function methods:SetFontObject(value) self.font = value end
CreateFrame = function(_, name, parent, template) local f = newFrame(name) f.parent = parent f.template = template return f end
ChatFontNormal = {}
local clock = 10
GetTime = function() return clock end
local errors, completed, chat = {}, {}, {}
UIErrorsFrame = {AddMessage=function(_, message, r, g, b) errors[#errors+1] = {message,r,g,b} end}
DEFAULT_CHAT_FRAME = {AddMessage=function(_, message) chat[#chat+1] = message end}
RaidWarningFrame = {}
RaidNotice_AddMessage = function(frame, message, color) assert(frame == RaidWarningFrame) completed[#completed+1] = {message,color} end
SendChatMessage = function() error("must never send chat") end
UIParent = newFrame("UIParent")
UISpecialFrames, SlashCmdList = {}, {}
BackdropTemplateMixin = {}
local messages = {}
print = function(message) messages[#messages+1] = message end
SetBinding = function() error("must never overwrite bindings") end
SetOverrideBinding = SetBinding
NanocodexWowDB = { hidden=true, position={point="TOPLEFT", relativePoint="TOPLEFT", x=10, y=-20} }
local NS = {}
assert(loadfile("addon/Nanocodex/Context.lua"))("Nanocodex", NS)
assert(loadfile("addon/Nanocodex/Core.lua"))("Nanocodex", NS)
local eventFrame
for _, f in ipairs(frames) do if f.events.ADDON_LOADED then eventFrame = f end end
assert(eventFrame)
eventFrame.scripts.OnEvent(eventFrame, "ADDON_LOADED", "OtherAddon")
assert(NanocodexWowPanel == nil)
eventFrame.scripts.OnEvent(eventFrame, "ADDON_LOADED", "Nanocodex")
assert(not NanocodexWowPanel:IsShown())
assert(NanocodexWowPanel.template == "BackdropTemplate")
assert(NanocodexWowPanel.backdrop.bgFile == "Interface\\DialogFrame\\UI-DialogBox-Background")
assert(NanocodexWowPanel.backdrop.edgeFile == "Interface\\DialogFrame\\UI-DialogBox-Border")
assert(NanocodexWowPanel.point[1] == "TOPLEFT")
assert(eventFrame.events.ADDON_LOADED == nil)
assert(SLASH_NANOCODEXWOW1 == "/nc" and SLASH_NANOCODEXWOW2 == "/nanocodex")
SlashCmdList.NANOCODEXWOW("show")
assert(NanocodexWowPanel:IsShown() and NanocodexWowDB.hidden == false)
SlashCmdList.NANOCODEXWOW("capture")
assert(#completed == 1 and completed[1][2].r == 1 and completed[1][2].g == 0.82)
SlashCmdList.NANOCODEXWOW("capture")
assert(#completed == 1, "repeat notices must be suppressed")
assert(NanocodexWowDB.lastContext.schemaVersion == 1)
assert(NanocodexWowDB.lastExportJson:find('"quests":%[%]'))
SlashCmdList.NANOCODEXWOW("reset")
assert(NanocodexWowDB.position == nil and NanocodexWowPanel.point[1] == "CENTER")
NanocodexWowPanel:SetPoint("BOTTOMLEFT", UIParent, "BOTTOMLEFT", 40, 50)
NanocodexWowPanel.scripts.OnDragStop(NanocodexWowPanel)
assert(NanocodexWowDB.position.x == 40 and NanocodexWowDB.position.y == 50)
local old = NanocodexWowDB.lastExportJson
NS.Capture = function() error("restricted capture") end
assert(NS.Snapshot() == nil and NanocodexWowDB.lastExportJson == old)
assert(#errors == 1 and errors[1][2] == 1 and errors[1][3] == 0.1)
SlashCmdList.NANOCODEXWOW("clear")
assert(NanocodexWowDB.lastContext == nil and NanocodexWowDB.lastExportJson == nil)
NS.Toggle()
assert(not NanocodexWowPanel:IsShown() and NanocodexWowDB.hidden == true)
NS.Toggle()
assert(NanocodexWowPanel:IsShown())
io.write("PASS: lifecycle, restore/drag/reset, slash commands, clear, capture failure, optional bindings\n")

SlashCmdList.NANOCODEXWOW("projects")
assert(#chat == 1 and chat[1]:find("|cffffd100Nanocodex:|r", 1, true))
SlashCmdList.NANOCODEXWOW("settings")
assert(#errors == 2, "missing settings must be guarded")
RaidNotice_AddMessage = nil
local info = {}
UIInfoMessageFrame = {AddMessage=function(_, message) info[#info+1] = message end}
NS.Notify("success", "Fallback completed")
assert(#info == 1)
UIInfoMessageFrame, UIErrorsFrame = nil, nil
NS.Notify("error", "Fallback error")
assert(#chat == 2)
clock = clock + 4
NS.Notify("error", "Fallback error")
assert(#chat == 3, "notices can repeat after throttle expires")
io.write("PASS: native dialog assets/fonts/templates, semantic local notices, throttling, API fallbacks\n")

function methods:GetText() return self.value or "" end
function methods:SetVerticalScroll(value) self.offset = value end
assert(loadfile("addon/Nanocodex/Projects.lua"))("Nanocodex", NS)
assert(loadfile("addon/Nanocodex/Bridge.lua"))("Nanocodex", NS)
local snapshot = "ncw1\nP\tp%2F1\tProject%20%7Cname\nT\tp%2F1\tt1\tChat%20one\trunning\nT\tp%2F1\tt2\tSecond\tidle\nP\tp2\tOther\n"
assert(NS.ImportProjects(snapshot))
SlashCmdList.NANOCODEXWOW("projects")
local projects = NanocodexWowProjects
assert(projects:IsShown() and #projects.rows == 2)
projects.rows[1].scripts.OnClick(projects.rows[1])
assert(#projects.rows == 4 and projects.rows[1].value:find("||", 1, true))
projects.rows[2].scripts.OnClick(projects.rows[2])
local p,t = NS.ProjectSelection()
assert(p == "p/1" and t == "t1")
assert(NanocodexWowDB.project_id == p and NanocodexWowDB.thread_id == t)
local previous = NanocodexWowDB.projectSnapshot
for _, bad in ipairs({"", "ncw2", "ncw1\nP\ta\t%", "ncw1\nP\ta\t%GG", "ncw1\nP\ta\t%00", "ncw1\nP\ta\tname\textra", "ncw1\nT\tmissing\tt\tTitle\tidle", "ncw1\nP\ta\tA\nP\ta\tB", "ncw1\nP\ta\tA\nT\ta\tt\tX\ti\nT\ta\tt\tY\ti", "ncw1\nP\t\tName", "ncw1\n\n", string.rep("x", 262145)}) do
    assert(not NS.ImportProjects(bad), bad)
    assert(NanocodexWowDB.projectSnapshot == previous)
end
local exact = "ncw1\nP\ta\t" .. string.rep("x", 262144 - #"ncw1\nP\ta\t")
assert(NS.ParseProjects(exact))
assert(not NS.ParseProjects(exact .. "x"))
local many = {"ncw1"}
for i=1,1000 do many[#many+1] = "P\tp"..i.."\tName" end
assert(NS.ParseProjects(table.concat(many,"\n")))
many[#many+1] = "P\textra\tName"
assert(not NS.ParseProjects(table.concat(many,"\n")))
assert(NS.ParseProjects("ncw1\r\nP\ta\tCaf%C3%A9%20%2520%2B\r\n").projects[1].name == "Café %20+")
-- Reload the module with SavedVariables intact, as at the next login.
assert(loadfile("addon/Nanocodex/Projects.lua"))("Nanocodex", NS)
p,t = NS.ProjectSelection()
assert(p == "p/1" and t == "t1")
local exports = {}
NS.Export = function() error("UI must never export to clipboard") end
assert(not NS.Ask("transport missing"))
assert(NS.TransportDisplay():find("Disconnected",1,true))
NS.TransportSend = function(value) exports[#exports+1] = value return true end
NS.TransportStatus = function() return {connected=false, state="offline"} end
NS.Capture = function() return {character={name="Tester"},specialization={},location={},target={}} end
NS.Ask("  hello  ")
local payload = exports[#exports]
assert(payload:find('"type":"nanocodex.ask"',1,true))
assert(payload:find('"mode":"agent"',1,true) and payload:find('"project_id":"p/1"',1,true))
assert(payload:find('"thread_id":"t1"',1,true) and payload:find('"prompt":"hello"',1,true))
assert(payload:find('"context":',1,true))
for _, action in ipairs({"create_project", "create_chat", "rename_project", "rename_chat"}) do
    assert(NS.ProjectAction(action, 'New "name"'))
    payload = exports[#exports]
    assert(payload:find('"type":"nanocodex.action"',1,true) and payload:find('"action":"'..action..'"',1,true))
    assert((payload:find('"project_id":',1,true) ~= nil) == (action ~= "create_project"))
    assert((payload:find('"thread_id":',1,true) ~= nil) == (action == "rename_chat"))
end
local count = #exports
assert(not NS.ProjectAction("delete_project", "Name"))
assert(not NS.ProjectAction("create_project", " "))
assert(not NS.ProjectAction("create_project", string.rep("x",513)))
assert(not NS.SelectProject("p2", "t1"))
assert(NS.SelectProject("p2"))
assert(not NS.ProjectAction("rename_chat", "Name"))
NS.Ask("hello")
assert(#exports == count)
assert(NS.ImportProjects("ncw1"))
p,t = NS.ProjectSelection()
assert(not p and not t)
assert(not NS.ProjectAction("create_chat", "Name"))
assert(NS.ProjectAction("create_project", "Name"))
NS.Ask("lore", true)
assert(exports[#exports]:find('"mode":"hint"',1,true))
assert(not exports[#exports]:find('"thread_id":',1,true))
NS.Ask("game fallback")
assert(exports[#exports]:find('"mode":"hint"',1,true))
io.write("PASS: snapshot bounds/atomic validation, native browse/select, persistence, ask routing, transport action payloads\n")

assert(NS.ParseProjects("ncw1\nP    root    My%20Project\nT    root    chat    Test%20chat    Ready"))

-- Delivery and failure semantics: queue receipt must not claim connection/success.
local sends = 0
NS.TransportSend = function() sends = sends + 1 return false, "pending" end
assert(NS.Ask("queued offline"))
assert(sends == 1, "pending must never automatically retry")
assert(NS.TransportDisplay():find("Disconnected",1,true))
assert(NS.TransportDisplay():find("Queued",1,true))
NS.OnTransportMessage("transport_ack", "1")
assert(NS.TransportDisplay():find("Transport acknowledged",1,true))
assert(NS.TransportDisplay():find("Disconnected",1,true))
NS.TransportStatus = function() return {connected=true, state="ready"} end
assert(NS.TransportDisplay():find("Bridge linked",1,true))
NS.TransportStatus = function() error("unavailable") end
assert(NS.TransportDisplay():find("Disconnected",1,true))
NS.TransportSend = function() return true end
assert(NS.Ask("acknowledged"))
assert(NS.TransportDisplay():find("Transport acknowledged",1,true))
NS.TransportSend = function() return false, "busy" end
assert(not NS.Ask("different request while busy"))
NS.TransportSend = function() return false, "queue full" end
assert(not NS.Ask("full"))
assert(NS.TransportDisplay():find("Not queued",1,true))
NS.TransportSend = function() error("unavailable") end
assert(not NS.ProjectAction("create_project", "Failure"))
NS.OnTransportMessage("reply", "Hello |Hbad|hworld|h\nnext\1")
NS.FlushConversation(0.05)
assert(NanocodexWowPanel:IsShown())
assert(NanocodexWowReply == nil, "answers belong in the main conversation")
assert(NanocodexWowPanel.answer:GetText() == "Hello ||Hbad||hworld||h\nnext")
assert(not NanocodexWowPanel.answer.focus)
NS.OnTransportMessage("projects", snapshot)
assert(NS.SelectProject("p/1", "t1"))
NS.OnTransportMessage("projects", "ncw1\nP\tbroken\t%GG")
assert(NS.ProjectSelection() == "p/1", "invalid update must preserve selection")
NS.OnTransportMessage("error", "Denied |cffff0000")
assert(NS.TransportDisplay():find("Denied ||cffff0000",1,true))
NS.OnTransportMessage("unknown", "ignored")
NS.OnTransportMessage("reply", {})
for _, f in ipairs(frames) do
    if f.value then
        assert(not f.value:find("Paste reply",1,true))
        assert(not f.value:find("Ask / Copy",1,true))
        assert(not f.value:find("Import snapshot",1,true))
    end
end
NanocodexWowPanel.scripts.OnUpdate(NanocodexWowPanel, 1)
io.write("PASS: missing/offline/failed transport, queued versus ack, automatic replies/snapshots, markup safety, no manual controls\n")

local answerBefore = NanocodexWowPanel.answer:GetText()
assert(NS.Reply(string.rep("x",262145)) == false)
assert(NanocodexWowPanel.answer:GetText() == answerBefore)
-- Minimize and hide retain drafts; passive replies never reopen or focus.
NanocodexWowPrompt:SetText("unfinished draft")
NanocodexWowPrompt:SetFocus()
NanocodexWowPanel.minimize.scripts.OnClick()
assert(NanocodexWowDB.minimized and not NanocodexWowPrompt:IsShown())
NS.OnTransportMessage("reply", "Minimized update")
NS.FlushConversation(0.05)
assert(NanocodexWowDB.minimized and NanocodexWowDB.draft == "unfinished draft")
NanocodexWowPanel:Hide()
NS.OnTransportMessage("reply", "Hidden update")
NS.FlushConversation(0.05)
assert(not NanocodexWowPanel:IsShown() and NanocodexWowDB.hidden)
assert(not NanocodexWowPanel.answer.focus and not NanocodexWowPrompt.focus)
NS.Reply()
assert(NanocodexWowPanel:IsShown() and not NanocodexWowDB.minimized)
assert(NanocodexWowPrompt:GetText() == "unfinished draft")
-- Real transport application + UI integration, without screen/input adapters.
assert(loadfile("addon/Transport.lua"))("Nanocodex", NS)
local T = NS.Transport
local link, peer = T.New(789), T.New(789, function() return true end)
T.app = T.NewApplication(link, function() return clock end, function(kind, value)
    return NS.OnTransportMessage(kind, value)
end)
local transportSend, sendCalls = NS.TransportSend, 0
NS.TransportSend = function(value, newRequest) sendCalls = sendCalls + 1 return transportSend(value, newRequest) end
local request = {type="nanocodex.ask",prompt=string.rep("q",240)}
local queued, state = NS.QueueRequest(request)
assert(queued and state == "pending")
assert(NS.TransportDisplay():find("Queued",1,true))
assert(NS.TransportDisplay():find("Disconnected",1,true))
local id = T.app.request.id
assert(not NS.QueueRequest({type="nanocodex.ask",prompt="different"}))
assert(T.app.request.id == id and NS.TransportDisplay():find("Queued",1,true))
assert(peer:Receive(link:Packet()))
assert(link:Receive(peer:Packet()))
assert(NS.TransportDisplay():find("Bridge linked",1,true))
assert(NS.TransportDisplay():find("Queued",1,true), "partial ACK must remain queued")
local callsBeforeStatus = sendCalls
while not T.app.request.done do
    assert(peer:Receive(link:Packet()))
    assert(link:Receive(peer:Packet()))
    NS.TransportDisplay()
end
assert(sendCalls == callsBeforeStatus, "status refresh must not resubmit")
assert(NS.TransportDisplay():find("Transport acknowledged",1,true))
assert(NS.QueueRequest(request))
assert(T.app.request.id == id + 1, "explicit identical action needs a fresh message identity")
assert(not NS.QueueRequest(request), "a second click while pending must be busy")
while not T.app.request.done do
    assert(peer:Receive(link:Packet()))
    assert(link:Receive(peer:Packet()))
    NS.TransportDisplay()
end
clock = clock + 11
assert(NS.TransportDisplay():find("Disconnected",1,true), "peer connection must expire")
local function deliver(kind, messageID, value)
    local offset = 0
    repeat
        local chunk = value:sub(offset+1,offset+88)
        assert(peer:Send(T.MessageChunk(kind,messageID,#value,offset,chunk)))
        assert(link:Receive(peer:Packet()))
        assert(peer:Receive(link:Packet()))
        offset = offset + #chunk
    until offset == #value
end
deliver("A",1,"Queued locally; awaiting remote acceptance.")
assert(NS.TransportDisplay():find("Queued locally; awaiting remote acceptance.",1,true))
deliver("A",2,"Accepted remotely; awaiting reply.")
assert(NS.TransportDisplay():find("Accepted remotely; awaiting reply.",1,true))
deliver("A",3,"Organization action completed.")
assert(NS.TransportDisplay():find("Organization action completed.",1,true))
deliver("R",4,"Automatic |reply")
NS.FlushConversation(0.05)
assert(NanocodexWowPanel.answer:GetText() == "Automatic ||reply")
deliver("P",5,snapshot)
assert(NS.SelectProject("p/1","t1"))
assert(NS.TransportDisplay():find("Project snapshot received",1,true))
io.write("PASS: real transport pending/busy, partial/final ACK, no resubmit, peer expiry, framed reply/snapshot delivery\n")

-- Rejected application data must not receive a carrier delivery receipt.
local rejectedLink=T.New(901)
local rejectedApp=T.NewApplication(rejectedLink,function() return clock end,NS.OnTransportMessage)
local malformed='not an ncw1 project snapshot'
local malformedPacket=T.Encode(901,1,0,T.MessageChunk('P',1,#malformed,0,malformed),true)
assert(not rejectedLink:Receive(malformedPacket))
assert(rejectedLink.rx==0 and rejectedApp.lastID==0)
assert(not rejectedLink:Receive(malformedPacket), 'replay must not turn a rejected snapshot into success')
assert(rejectedLink.rx==0 and rejectedApp.lastID==0)
assert(NS.OnTransportMessage('unknown','value')==false)
assert(NS.OnTransportMessage('reply',{})==false)
assert(NS.OnTransportMessage('reply',string.rep('x',256*1024+1))==false)
local importer=NS.ImportProjects NS.ImportProjects=nil
assert(NS.OnTransportMessage('projects',snapshot)==false)
NS.ImportProjects=importer
io.write("PASS: rejected snapshots/replies retain unacknowledged carrier and application identities\n")

-- Slash entry before ADDON_LOADED must open the newly constructed panel.
local fresh = {}
assert(loadfile("addon/Nanocodex/Core.lua"))("Nanocodex", fresh)
SlashCmdList.NANOCODEXWOW("")
assert(NanocodexWowPanel:IsShown())
SlashCmdList.NANOCODEXWOW("")
assert(not NanocodexWowPanel:IsShown())
io.write("PASS: bounded automatic reply and first slash entry\n")
