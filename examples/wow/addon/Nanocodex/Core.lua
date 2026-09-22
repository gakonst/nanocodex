local ADDON, NS = ...
local panel, summary, transportLabel
-- Local UI notices only; never send a chat message or raid broadcast.
local lastNotice = {}
function NS.Notify(kind, message)
    local now = type(GetTime) == "function" and GetTime() or nil
    local key = kind .. ":" .. message
    if now and lastNotice[key] and now - lastNotice[key] < 3 then return end
    if now then
        for previous, time in pairs(lastNotice) do
            if now - time >= 3 then lastNotice[previous] = nil end
        end
        lastNotice[key] = now
    end
    local value = "Nanocodex: " .. message
    if kind == "error" and UIErrorsFrame and UIErrorsFrame.AddMessage then
        UIErrorsFrame:AddMessage(value, 1, 0.1, 0.1)
    elseif kind == "success" and type(RaidNotice_AddMessage) == "function" and RaidWarningFrame then
        RaidNotice_AddMessage(RaidWarningFrame, value, {r=1, g=0.82, b=0})
    elseif kind == "success" and UIInfoMessageFrame and UIInfoMessageFrame.AddMessage then
        UIInfoMessageFrame:AddMessage(value, 1, 0.82, 0)
    elseif DEFAULT_CHAT_FRAME and DEFAULT_CHAT_FRAME.AddMessage then
        DEFAULT_CHAT_FRAME:AddMessage("|cffffd100Nanocodex:|r " .. message)
    else
        print("|cffffd100Nanocodex:|r " .. message)
    end
end
local function say(message) NS.Notify("status", message) end
local function db()
    if type(NanocodexWowDB) ~= "table" then NanocodexWowDB = {} end
    return NanocodexWowDB
end
local function text(parent, value, size, x, y, width)
    local label = parent:CreateFontString(nil, "OVERLAY", size or "GameFontNormal")
    label:SetPoint("TOPLEFT", x, y)
    label:SetWidth(width)
    label:SetJustifyH("LEFT")
    label:SetText(value)
    return label
end
local function surface(name, width, height)
    local frame = CreateFrame("Frame", name, UIParent, BackdropTemplateMixin and "BackdropTemplate" or nil)
    frame:SetSize(width, height)
    frame:SetPoint("CENTER")
    frame:SetFrameStrata("DIALOG")
    frame:SetClampedToScreen(true)
    frame:EnableMouse(true)
    frame:SetMovable(true)
    frame:RegisterForDrag("LeftButton")
    frame:SetScript("OnDragStart", frame.StartMoving)
    frame:SetScript("OnDragStop", function(self) self:StopMovingOrSizing() end)
    if frame.SetBackdrop then
        frame:SetBackdrop({
            bgFile = "Interface\\DialogFrame\\UI-DialogBox-Background",
            edgeFile = "Interface\\DialogFrame\\UI-DialogBox-Border",
            tile = true, tileSize = 32, edgeSize = 32,
            insets = { left = 11, right = 12, top = 12, bottom = 11 },
        })
        frame:SetBackdropColor(1, 1, 1, 1)
        frame:SetBackdropBorderColor(1, 1, 1, 1)
    end
    local close = CreateFrame("Button", nil, frame, "UIPanelCloseButton")
    close:SetPoint("TOPRIGHT", 0, 0)
    close:SetScript("OnClick", function() frame:Hide() end)
    return frame
end
local function button(parent, label, x, callback)
    local b = CreateFrame("Button", nil, parent, "UIPanelButtonTemplate")
    b:SetSize(92, 24)
    b:SetPoint("BOTTOMLEFT", x, 12)
    b:SetText(label)
    b:SetScript("OnClick", callback)
    return b
end
local function updateSummary(context)
    if not summary then return end
    local c = context.character
    summary:SetText(((c.name or "Character unavailable") .. " • " .. (context.location.zone or "Unknown zone")):gsub("|", "||"))
end
function NS.Snapshot()
    local ok, context = pcall(NS.Capture)
    if not ok then NS.Notify("error", "Context unavailable; please try again outside combat.") return nil end
    local encodedOK, encoded = pcall(NS.Encode, context)
    if not encodedOK then NS.Notify("error", "Context could not be encoded; please try again.") return nil end
    db().lastContext = context
    db().lastExportJson = encoded
    updateSummary(context)
    return encoded
end
NS.BridgeSurface = surface
NS.BridgeText = text
local function makePanel()
    if panel then return end
    panel = surface("NanocodexWowPanel", 460, 700)
    text(panel, "Nanocodex · threads", "GameFontNormalLarge", 16, -16, 280)
    panel:ClearAllPoints()
    panel:SetPoint("RIGHT", UIParent, "RIGHT", -12, 0)
    summary = text(panel, "Account sign-in stays in the desktop companion.", "GameFontHighlightSmall", 16, -48, 425)
    panel.conversationTitle = text(panel, "Select a thread, or ask WoW for a separate game conversation.", "GameFontHighlightSmall", 16, -318, 425)
    transportLabel = text(panel, "Disconnected · transport unavailable", "GameFontHighlightSmall", 16, -72, 425)
    local prompt = CreateFrame("EditBox", "NanocodexWowPrompt", panel, "InputBoxTemplate")
    prompt:SetSize(410, 30)
    prompt:SetPoint("TOPLEFT", 22, -613)
    prompt:SetAutoFocus(false)
    prompt:SetFontObject(ChatFontNormal or "GameFontHighlight")
    prompt:SetMaxLetters(2000)
    prompt:SetText(type(db().draft) == "string" and db().draft or "")
    prompt:SetScript("OnTextChanged", function(self) db().draft = self:GetText() end)
    prompt:SetScript("OnEscapePressed", function(self) self:ClearFocus() end)
    local function ask()
        if NS.Ask and NS.Ask(prompt:GetText()) then
            prompt:SetText("")
        end
        prompt:ClearFocus()
    end
    prompt:SetScript("OnEnterPressed", ask)
    panel.askButton = button(panel, "Ask", 224, ask)
    panel.wowButton = button(panel, "Ask WoW", 329, function() if NS.Ask and NS.Ask(prompt:GetText(), true) then prompt:SetText("") end prompt:ClearFocus() end)
    local scroll = CreateFrame("ScrollFrame", nil, panel, "UIPanelScrollFrameTemplate")
    scroll:SetPoint("TOPLEFT", 20, -346)
    scroll:SetPoint("BOTTOMRIGHT", -36, 92)
    local answer = CreateFrame("EditBox", nil, scroll)
    answer:SetMultiLine(true)
    answer:SetAutoFocus(false)
    answer:SetFontObject(ChatFontNormal or "GameFontHighlight")
    answer:SetWidth(400)
    answer:SetHeight(190)
    answer:SetMaxLetters(0)
    answer:SetScript("OnTextChanged", function(self, userInput)
        if userInput then self:SetText(panel.displayText or "Waiting for a reply.") end
        scroll:UpdateScrollChildRect()
    end)
    answer:SetScript("OnEscapePressed", function(self) self:ClearFocus() end)
    scroll:SetScrollChild(answer)
    panel.answer = answer
    panel.displayText = "Waiting for a reply."
    answer:SetText(panel.displayText)
    local minimize = CreateFrame("Button", nil, panel, "UIPanelButtonTemplate")
    minimize:SetSize(80, 20)
    minimize:SetPoint("TOPRIGHT", -30, -15)
    local function resize()
        local minimized = db().minimized == true
        panel:SetSize(460, minimized and 44 or 700)
        -- Preserve the usable layout on short displays and UI-scale changes.
        local height = UIParent:GetHeight()
        local width = UIParent:GetWidth()
        panel:SetScale(math.min(1, math.max(1, height - 24) / 700, math.max(1, width - 24) / 460))
        minimize:SetText(minimized and "Expand" or "Minimize")
        for _, child in ipairs({summary, transportLabel, prompt, scroll, panel.askButton, panel.wowButton, panel.projectsButton, panel.answerButton, panel.threadScroll, panel.threadSearch, panel.searchLabel, panel.refreshButton, panel.newButton, panel.olderButton, panel.stopButton, panel.reconnectButton}) do
            if minimized then child:Hide() else child:Show() end
        end
        if minimized then prompt:ClearFocus() answer:ClearFocus() if panel.threadSearch then panel.threadSearch:ClearFocus() end end
        if minimized then panel.conversationTitle:Hide() else panel.conversationTitle:Show() end
    end
    minimize:SetScript("OnClick", function() db().minimized = not db().minimized resize() end)
    panel.minimize = minimize
    panel.resize = resize
    panel.projectsButton = button(panel, "Projects", 14, function() if NS.Projects then NS.Projects() end end)
    panel.answerButton = button(panel, "Answer", 119, function() if NS.Reply then NS.Reply() end end)
    local function action(label, x, y, width, callback)
        local b = CreateFrame("Button", nil, panel, "UIPanelButtonTemplate")
        b:SetSize(width, 24) b:SetPoint("TOPLEFT", x, y) b:SetText(label) b:SetScript("OnClick", callback)
        return b
    end
    panel.searchLabel = text(panel, "Search loaded chats", "GameFontHighlightSmall", 18, -113, 300)
    local search = CreateFrame("EditBox", nil, panel, "InputBoxTemplate")
    search:SetSize(305, 24) search:SetPoint("TOPLEFT", 22, -128) search:SetAutoFocus(false)
    search:SetFontObject(ChatFontNormal or "GameFontHighlight") search:SetMaxLetters(120)
    search:SetScript("OnEscapePressed", function(self) self:ClearFocus() end)
    search:SetScript("OnTextChanged", function() if NS.RefreshThreadList then NS.RefreshThreadList() end end)
    panel.threadSearch = search
    panel.refreshButton = action("Refresh", 340, -128, 96, function() if NS.RefreshWorkspace then NS.RefreshWorkspace() end search:ClearFocus() end)
    local threadScroll = CreateFrame("ScrollFrame", nil, panel, "UIPanelScrollFrameTemplate")
    threadScroll:SetPoint("TOPLEFT", 18, -159) threadScroll:SetSize(400, 114)
    local threadChild = CreateFrame("Frame", nil, threadScroll) threadChild:SetSize(395, 114) threadScroll:SetScrollChild(threadChild)
    panel.threadScroll, panel.threadRows = threadScroll, {}
    local entries = {}
    local ROW_HEIGHT, VISIBLE_ROWS = 26, math.ceil(114 / 26) + 1
    local function paintThreads()
        local _, selected
        if NS.ProjectSelection then _, selected = NS.ProjectSelection() end
        local first = math.floor(threadScroll:GetVerticalScroll() / ROW_HEIGHT) + 1
        for slot=1,VISIBLE_ROWS do
            local index = first + slot - 1
            local entry, row = entries[index], panel.threadRows[slot]
            if entry then
                if not row then
                    row = CreateFrame("Button", nil, threadChild, "UIPanelButtonTemplate")
                    row:SetSize(390, 24)
                    row:SetScript("OnClick", function(self)
                        if NS.LoadThread then NS.LoadThread(self.projectID, self.threadID)
                        else NS.SelectProject(self.projectID, self.threadID) end
                        search:ClearFocus() prompt:ClearFocus()
                    end)
                    panel.threadRows[slot] = row
                end
                row:ClearAllPoints() row:SetPoint("TOPLEFT", 0, -(index-1)*ROW_HEIGHT)
                row.projectID, row.threadID = entry.project_id, entry.id
                row:SetText(((entry.id == selected and "> " or "") .. entry.title .. (entry.status == "closed" and " [closed]" or "") .. " · " .. entry.project_name):gsub("|", "||")) row:Show()
            elseif row then
                row.projectID, row.threadID = nil, nil
                row:Hide()
            end
        end
    end
    threadScroll:SetScript("OnVerticalScroll", function(self, offset)
        self:SetVerticalScroll(offset)
        paintThreads()
    end)
    local lastQuery
    panel.refreshThreads = function()
        local query = search:GetText()
        entries = NS.ThreadEntries and NS.ThreadEntries(query) or {}
        local height = math.max(114, #entries*ROW_HEIGHT)
        threadChild:SetHeight(height) threadScroll:UpdateScrollChildRect()
        local offset = query ~= lastQuery and 0 or math.min(threadScroll:GetVerticalScroll(), height - 114)
        lastQuery = query
        threadScroll:SetVerticalScroll(offset)
        paintThreads()
        local title = NS.SelectedThreadTitle and NS.SelectedThreadTitle()
        panel.conversationTitle:SetText(title and title:gsub("|", "||") or "Select a thread, or ask WoW for a separate game conversation.")
    end
    panel.newButton = action("New chat", 16, -283, 96, function()
        if NS.ProjectAction then NS.ProjectAction("create_chat", "New conversation") end prompt:ClearFocus()
    end)
    panel.olderButton = action("Earlier", 120, -283, 96, function() if NS.LoadEarlier then NS.LoadEarlier() end prompt:ClearFocus() end)
    panel.stopButton = action("Stop", 224, -283, 96, function() if NS.StopThread then NS.StopThread() end prompt:ClearFocus() end)
    panel.reconnectButton = action("Reconnect", 328, -283, 108, function() if NS.ReconnectClient then NS.ReconnectClient() end prompt:ClearFocus() end)
    panel.refreshThreads()
    resize()
    panel:SetScript("OnUpdate", function(self, elapsed)
        self.statusElapsed = (self.statusElapsed or 0) + elapsed
        if self.statusElapsed < 0.25 then return end
        self.statusElapsed = 0
        if NS.TransportDisplay then transportLabel:SetText(NS.TransportDisplay()) end
    end)
    panel:SetScript("OnDragStop", function(self)
        self:StopMovingOrSizing()
        local point, _, relativePoint, x, y = self:GetPoint(1)
        db().position = { point = point, relativePoint = relativePoint, x = x, y = y }
    end)
    panel:SetScript("OnHide", function() db().hidden = true prompt:ClearFocus() answer:ClearFocus() search:ClearFocus() end)
    panel:SetScript("OnShow", function() db().hidden = false resize() end)
    local p = db().position
    local anchors = { CENTER=true, TOP=true, BOTTOM=true, LEFT=true, RIGHT=true, TOPLEFT=true, TOPRIGHT=true, BOTTOMLEFT=true, BOTTOMRIGHT=true }
    if type(p) == "table" and anchors[p.point] and anchors[p.relativePoint] and type(p.x) == "number" and type(p.y) == "number" then
        panel:ClearAllPoints()
        panel:SetPoint(p.point, UIParent, p.relativePoint, p.x, p.y)
    end
    table.insert(UISpecialFrames, "NanocodexWowPanel")
end
function NS.RefreshThreadList() if panel and panel.refreshThreads then panel.refreshThreads() end end
-- Passive answer updates never show the panel or touch keyboard focus.
function NS.DisplayConversation(value)
    if not panel then makePanel() panel:Hide() end
    panel.displayText = value
    panel.answer:SetText(value)
end
function NS.OpenConversation()
    makePanel()
    db().minimized = false
    panel.resize()
    panel:Show()
end
function NS.Toggle()
    if not panel then makePanel() panel:Show() return end
    if panel:IsShown() then panel:Hide() else panel:Show() end
end
local function slash(message)
    local command = (message or ""):lower():match("^%s*(%S*)")
    if command == "" or command == "toggle" then NS.Toggle()
    elseif command == "show" then makePanel() panel:Show()
    elseif command == "hide" then if panel then panel:Hide() end
    elseif command == "game" then if NS.Ask then NS.Ask((message or ""):match("^%s*%S+%s*(.*)$"), true) end
    elseif command == "ask" then
        if NS.Ask then NS.Ask((message or ""):match("^%s*%S+%s*(.*)$")) end
    elseif command == "reply" then
        if NS.Reply then NS.Reply() end
    elseif command == "capture" then if NS.Snapshot() then NS.Notify("success", "Captured in memory; logout or /reload writes SavedVariables.") end
    elseif command == "reset" then makePanel() db().position = nil panel:ClearAllPoints() panel:SetPoint("CENTER") panel:Show()
    elseif command == "clear" then db().lastContext = nil db().lastExportJson = nil NS.Notify("success", "Stored snapshot cleared in memory; logout or /reload persists this change.")
    elseif command == "settings" then
        if type(NS.Settings) == "function" then NS.Settings((message or ""):match("^%s*%S+%s*(.*)$"))
        else NS.Notify("error", "Settings are unavailable in this installation.") end
    elseif command == "bridge-debug" then
        NS.Notify("status", NS.Transport and NS.Transport.DebugStatus and NS.Transport.DebugStatus() or "Transport diagnostics unavailable.")
    elseif command == "bridge" then
        local argument = (message or ""):match("^%s*%S+%s*(.-)%s*$")
        local transport = NS.Transport
        if not transport then NS.Notify("error", "Transport module unavailable.") return end
        if argument == "off" then
            local _, err = transport.Disable()
            NS.Notify(err and "error" or "status", err or "Bridge disabled.")
            return
        end
        if argument == "on" or argument == "auto" then
            local ok, detail = transport.StartAutomatic()
            NS.Notify(ok and "status" or "error", detail)
            return
        end
        local sessionText, mode = argument:match('^(%S+)%s+(%S+)$')
        if not sessionText then sessionText = argument end
        mode = mode or "chord"
        local session = tonumber(sessionText)
        if (mode and mode ~= 'chord' and mode ~= 'function') or not session or session < 1 or session > 4294967295 or session ~= math.floor(session) then
            NS.Notify("error", "Use /nc bridge on, /nc bridge off, or /nc bridge <session number> [chord|function].") return
        end
        -- Let the slash-command edit box release focus before checking input guards.
        if not C_Timer or not C_Timer.After then NS.Notify("error", "Deferred setup unavailable.") return end
        C_Timer.After(0.1, function()
            local link, err = transport.Enable(session, nil, mode)
            NS.Notify(link and "status" or "error", link and "Bridge enabled; awaiting peer. Authentication unverified." or tostring(err))
        end)
    elseif command == "threads" then NS.OpenConversation() if NS.RefreshWorkspace then NS.RefreshWorkspace() end
    elseif command == "projects" then if NS.Projects then NS.Projects() else say("Project browser unavailable.") end
    else say("/nc [show | hide | ask <question> | game <question> | reply | threads | projects | capture | reset | clear | settings]") end
end
SLASH_NANOCODEXWOW1 = "/nc"
SLASH_NANOCODEXWOW2 = "/nanocodex"
SlashCmdList.NANOCODEXWOW = slash
BINDING_HEADER_NANOCODEXWOW = "Nanocodex"
BINDING_NAME_NANOCODEXWOW_TOGGLE = "Toggle companion panel"
local events = CreateFrame("Frame")
events:RegisterEvent("ADDON_LOADED")
events:RegisterEvent("UI_SCALE_CHANGED")
events:RegisterEvent("DISPLAY_SIZE_CHANGED")
-- This frame remains shown even when the conversation panel is hidden. Read
-- continuations and passive stream rendering must not depend on panel visibility.
events:SetScript("OnUpdate", function(_, elapsed)
    if NS.FlushConversation then NS.FlushConversation(elapsed) end
    if NS.PumpClient then NS.PumpClient() end
end)
events:SetScript("OnEvent", function(self, event, name)
    if event ~= "ADDON_LOADED" then
        if panel then panel.resize() end
        return
    end
    if name ~= ADDON then return end
    local hidden = db().hidden
    makePanel()
    if hidden then panel:Hide() end
    self:UnregisterEvent("ADDON_LOADED")
end)
