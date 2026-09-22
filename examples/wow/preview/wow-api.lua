-- Browser host for the real addon. Only WoW UI APIs and transport are adapted.
-- Unknown methods deliberately fail instead of silently accepting unsupported APIs.
NS = {}
local frames, requests, notices = {}, {}, {}
local clock, started = 0, false
local connected, session, messageID = false, 1, 0
local pending, pendingCount = {}, 0
local methods = {}
local function finite(value)
    return type(value) == "number" and value == value and value > -math.huge and value < math.huge
end
local function frameByID(id)
    local frame = frames[tonumber(id)]
    assert(frame, "Unknown preview frame: " .. tostring(id))
    return frame
end
local function callback(frame, event, ...)
    local fn = frame.scripts[event]
    if fn then return fn(frame, ...) end
end
local function newFrame(kind, name, parent, template)
    local frame = setmetatable({id=#frames+1, kind=kind, name=name, parent=parent,
        template=template, scripts={}, events={}, points={}, visible=true,
        width=0, height=0, scale=1, value="", offset=0, focus=false}, {__index=function(_, key)
            if methods[key] then return methods[key] end
            if type(key)=="string" and key:match("^[A-Z]") then error("Unsupported WoW frame method: " .. key, 2) end
        end})
    frames[#frames+1] = frame
    if name then _G[name] = frame end
    if template == "UIPanelCloseButton" then frame.width, frame.height = 32, 32 end
    return frame
end
function CreateFrame(kind, name, parent, template) return newFrame(kind, name, parent, template) end
function methods:SetScript(event, fn) assert(fn == nil or type(fn)=="function") self.scripts[event] = fn end
function methods:GetScript(event) return self.scripts[event] end
function methods:RegisterEvent(event) self.events[event] = true end
function methods:UnregisterEvent(event) self.events[event] = nil end
function methods:CreateFontString(name, layer, font)
    local frame = newFrame("FontString", name, self)
    frame.font, frame.layer = font, layer
    return frame
end
function methods:SetPoint(point, relative, relativePoint, x, y)
    -- SetPoint(point), (point,x,y), (point,relative,x,y), and full form.
    if type(relative)=="number" then
        x, y, relative, relativePoint = relative, relativePoint, self.parent or UIParent, point
    else
        relative = relative or self.parent or UIParent
        if type(relative)=="string" then relative = assert(_G[relative], "Unknown relative frame") end
        if type(relativePoint)=="number" then x, y, relativePoint = relativePoint, x, point end
        relativePoint = relativePoint or point
    end
    x, y = x or 0, y or 0
    assert(finite(x) and finite(y), "Non-finite SetPoint coordinate")
    local anchor = {point=point, relative=relative, relativePoint=relativePoint, x=x, y=y}
    for index, existing in ipairs(self.points) do
        if existing.point == point then self.points[index] = anchor return end
    end
    self.points[#self.points+1] = anchor
end
function methods:GetPoint(index)
    local p = self.points[index or 1]
    if p then return p.point, p.relative, p.relativePoint, p.x, p.y end
end
function methods:ClearAllPoints() self.points = {} end
function methods:SetSize(width, height) self.width, self.height = width, height end
function methods:SetWidth(value) self.width = value end
function methods:SetHeight(value) self.height = value end
function methods:GetWidth() return self.width end
function methods:GetHeight() return self.height end
function methods:SetScale(value) self.scale = value end
function methods:GetScale() return self.scale end
function methods:IsShown() return self.visible end
function methods:IsVisible() return self.visible and (not self.parent or self.parent:IsVisible()) end
function methods:Show()
    local changed = not self.visible
    self.visible = true
    if changed then callback(self, "OnShow") end
end
function methods:Hide()
    local changed = self.visible
    self.visible = false
    if changed then callback(self, "OnHide") end
end
function methods:SetText(value)
    self.value = tostring(value or "")
    callback(self, "OnTextChanged", false)
end
function methods:GetText() return self.value end
function methods:SetFocus()
    for _, frame in ipairs(frames) do if frame ~= self and frame.focus then frame:ClearFocus() end end
    if not self.focus then self.focus = true callback(self, "OnEditFocusGained") end
end
function methods:ClearFocus()
    if self.focus then self.focus = false callback(self, "OnEditFocusLost") end
end
function methods:HighlightText() self.highlighted = true end
function methods:SetScrollChild(child) self.child = child child.parent = self end
function methods:GetVerticalScroll() return self.offset end
function methods:SetVerticalScroll(value) self.offset = value end
function methods:UpdateScrollChildRect() end
function methods:SetBackdrop(value) self.backdrop = value end
function methods:SetBackdropColor(...) self.backdropColor = {...} end
function methods:SetBackdropBorderColor(...) self.backdropBorderColor = {...} end
function methods:SetFontObject(value) self.font = value end
function methods:SetMultiLine(value) self.multiline = value end
function methods:SetAutoFocus(value) self.autoFocus = value end
function methods:SetMaxLetters(value) self.maxLetters = value end
function methods:SetJustifyH(value) self.justifyH = value end
function methods:SetFrameStrata(value) self.strata = value end
function methods:SetClampedToScreen(value) self.clamped = value end
function methods:EnableMouse(value) self.mouseEnabled = value end
function methods:SetMovable(value) self.movable = value end
function methods:RegisterForDrag(...) self.dragButtons = {...} end
function methods:StartMoving() self.moving = true end
function methods:StopMovingOrSizing() self.moving = false end

UIParent = newFrame("Frame", "UIParent")
UIParent:SetSize(1280, 900)
ChatFontNormal = "ChatFontNormal"
BackdropTemplateMixin = {}
UISpecialFrames, SlashCmdList = {}, {}
GetTime = function() return clock end
GetZoneText = function() return "Game context unavailable outside WoW" end
-- Unit, quest, map, realm and build APIs are intentionally unavailable.
local function notice(_, message) notices[#notices+1] = tostring(message) end
UIErrorsFrame, UIInfoMessageFrame, DEFAULT_CHAT_FRAME = {AddMessage=notice}, {AddMessage=notice}, {AddMessage=notice}
RaidWarningFrame = {}
RaidNotice_AddMessage = notice
print = function(...)
    local parts = {...}
    for i, value in ipairs(parts) do parts[i] = tostring(value) end
    notices[#notices+1] = table.concat(parts, "\t")
end

function PreviewLoad(source)
    assert(not started, "Load addon modules before PreviewStart")
    return assert(load(source, "@addon", "t"))("Nanocodex", NS)
end
function PreviewRestore(draft, hidden, minimized, x, y)
    assert(not started, "Restore SavedVariables before PreviewStart")
    NanocodexWowDB = {draft=type(draft)=="string" and draft or "", hidden=hidden==true, minimized=minimized==true}
    if x ~= nil and y ~= nil then
        NanocodexWowDB.position = {point="TOPLEFT", relativePoint="TOPLEFT", x=x, y=-y}
    end
end
local function dispatch(event, ...)
    for _, frame in ipairs(frames) do
        if frame.events[event] then callback(frame, "OnEvent", event, ...) end
    end
end
function PreviewStart()
    assert(not started, "Preview already started")
    started = true
    NS.TransportSend = function(payload)
        if not connected then return false, "Connect Nanocodex first" end
        if pendingCount >= 128 then return false, "Too many outstanding requests; wait for the companion" end
        messageID = messageID + 1
        local rid = string.format("ncw:%08x:%04x", session, messageID)
        pending[rid], pendingCount = payload, pendingCount + 1
        requests[#requests+1] = {payload=payload, id=rid}
        return false, "pending"
    end
    NS.TransportPending = function(payload)
        for _, queued in pairs(pending) do
            if queued == payload then return true end
        end
        return false
    end
    NS.TransportStatus = function()
        return {connected=connected, pending=false, inflight=pendingCount, session=session, message_id=messageID,
            state=connected and "Browser companion connected" or "Connect Nanocodex first"}
    end
    dispatch("ADDON_LOADED", "Nanocodex")
end
function PreviewLink(value, sessionNumber)
    assert(type(value)=="boolean", "Connection must be boolean")
    if sessionNumber ~= nil then
        -- Fengari uses signed 32-bit integers; Client.lua formats this identity too.
        assert(finite(sessionNumber) and sessionNumber%1==0 and sessionNumber>=1 and sessionNumber<=2147483647, "Invalid preview session")
        if sessionNumber ~= session then
            assert(pendingCount == 0, "Cannot replace a session with outstanding requests")
            session, messageID = sessionNumber, 0
        end
    end
    connected = value
end
function PreviewAccepted(rid)
    rid = rid or string.format("ncw:%08x:%04x", session, messageID)
    if pending[rid] then pending[rid], pendingCount = nil, pendingCount - 1 end
end
function PreviewReceive(kind, value) return NS.OnTransportMessage(kind, value) end
function PreviewCommand(value) return SlashCmdList.NANOCODEXWOW(value) end
function PreviewResize(width, height)
    assert(finite(width) and finite(height) and width>0 and height>0, "Invalid preview dimensions")
    UIParent:SetSize(width, height)
    dispatch("DISPLAY_SIZE_CHANGED")
    dispatch("UI_SCALE_CHANGED")
end
function PreviewTick(seconds)
    assert(finite(seconds) and seconds>=0, "Invalid elapsed time")
    clock = clock + seconds
    for _, frame in ipairs(frames) do
        if frame:IsVisible() then callback(frame, "OnUpdate", seconds) end
    end
end
local aliases = {input="OnTextChanged", scroll="OnVerticalScroll", focus="OnEditFocusGained", blur="OnEditFocusLost", enter="OnEnterPressed", escape="OnEscapePressed", click="OnClick"}
function PreviewEvent(id, event, value)
    local frame = frameByID(id)
    event = aliases[event] or event
    if event=="OnTextChanged" then
        frame.value = tostring(value or "")
        return callback(frame, event, true)
    elseif event=="OnEditFocusGained" then return frame:SetFocus()
    elseif event=="OnEditFocusLost" then return frame:ClearFocus()
    elseif event=="OnVerticalScroll" then
        assert(finite(value), "Invalid scroll offset")
        frame:SetVerticalScroll(value)
        return callback(frame, event, value)
    elseif event=="OnClick" then return callback(frame, event, value or "LeftButton") end
    return callback(frame, event, value)
end
function PreviewMove(id, x, y)
    local frame = frameByID(id)
    frame:ClearAllPoints()
    frame:SetPoint("TOPLEFT", UIParent, "TOPLEFT", x, -y)
    callback(frame, "OnDragStop")
end
function PreviewSnapshot()
    local result = {frames=setmetatable({}, NS.array), requests=setmetatable(requests, NS.array),
        notices=setmetatable(notices, NS.array), saved={}}
    -- The renderer only persists layout/draft fields. Never serialize the whole
    -- catalog or captured game context on every animation tick.
    for _, key in ipairs({"draft", "hidden", "minimized", "position", "thread_id", "project_id"}) do
        result.saved[key] = (NanocodexWowDB or {})[key]
    end
    for _, frame in ipairs(frames) do
        local points, scripts = setmetatable({}, NS.array), {}
        for _, p in ipairs(frame.points) do
            points[#points+1] = {point=p.point, relative=p.relative.id, relativePoint=p.relativePoint, x=p.x, y=p.y}
        end
        for key in pairs(frame.scripts) do scripts[key] = true end
        result.frames[#result.frames+1] = {id=frame.id, parent=frame.parent and frame.parent.id,
            kind=frame.kind, name=frame.name, template=frame.template, text=frame.questLabel or frame.value,
            width=frame.width, height=frame.height, points=points, visible=frame:IsVisible(),
            scale=frame.scale, multiline=frame.multiline==true, font=frame.font,
            offset=frame.offset, child=frame.child and frame.child.id, focus=frame.focus,
            backdrop=frame.backdrop~=nil, backdropColor=frame.backdropColor and setmetatable(frame.backdropColor, NS.array), movable=frame.movable==true, clamped=frame.clamped==true, questRow=frame.questRow==true, active=frame.active==true, scripts=scripts, threadID=frame.threadID, projectID=frame.projectID}
    end
    local encoded = NS.Encode(result)
    requests, notices = {}, {}
    return encoded
end
