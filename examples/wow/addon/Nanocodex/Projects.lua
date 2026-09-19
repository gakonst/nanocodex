local _, NS = ...
local MAX_BYTES, MAX_ROWS = 256 * 1024, 1000
local panel, state
local function db()
    if type(NanocodexWowDB) ~= "table" then NanocodexWowDB = {} end
    return NanocodexWowDB
end
local function decode(s)
    if s:gsub("%%[%x][%x]", ""):find("%%") then return nil end
    s = s:gsub("%%(%x%x)", function(h) return string.char(tonumber(h, 16)) end)
    if s:find("[%z\1-\31\127]") then return nil end
    return s
end
-- This is data, never executable Lua or WoW markup. Reject the entire import on error.
function NS.ParseProjects(value)
    if type(value) ~= "string" or #value > MAX_BYTES then return nil, "Snapshot exceeds 256 KiB." end
    value = value:gsub("\r\n", "\n")
    if value:sub(-1) == "\n" then value = value:sub(1, -2) end
    local result = {projects={}, threads={}, byProject={}, byThread={}}
    local count = 0
    for line in (value .. "\n"):gmatch("([^\n]*)\n") do
        if count == 0 then
            if line ~= "ncw1" then return nil, "Expected ncw1 snapshot header." end
        else
            if count > MAX_ROWS then return nil, "Snapshot exceeds 1000 rows." end
            local fields = {}
            if line:find("\t", 1, true) then
                for field in (line .. "\t"):gmatch("([^\t]*)\t") do fields[#fields+1] = field end
            else
                -- WoW's clipboard normalizes tab characters to spaces on some clients.
                -- Every field is percent-encoded, so literal whitespace is only a delimiter.
                for field in line:gmatch("%S+") do fields[#fields+1] = field end
            end
            local kind = fields[1]
            if (kind ~= "P" and kind ~= "T") or #fields ~= (kind == "P" and 3 or 5) then return nil, "Invalid snapshot row." end
            for i=2,#fields do
                fields[i] = decode(fields[i])
                if not fields[i] then return nil, "Invalid percent encoding or control character." end
            end
            if fields[2] == "" or (kind == "T" and fields[3] == "") then return nil, "Empty identifier." end
            if kind == "P" then
                if result.byProject[fields[2]] then return nil, "Duplicate project." end
                local p = {id=fields[2], name=fields[3]}
                result.projects[#result.projects+1], result.byProject[p.id] = p, p
            else
                local t = {project_id=fields[2], id=fields[3], title=fields[4], status=fields[5]}
                result.byThread[t.project_id] = result.byThread[t.project_id] or {}
                if result.byThread[t.project_id][t.id] then return nil, "Duplicate chat." end
                result.threads[#result.threads+1], result.byThread[t.project_id][t.id] = t, t
            end
        end
        count = count + 1
    end
    for _, t in ipairs(result.threads) do
        if not result.byProject[t.project_id] then return nil, "Chat references an unknown project." end
    end
    return result
end
local function current()
    if not state then state = NS.ParseProjects(db().projectSnapshot or "ncw1") or NS.ParseProjects("ncw1") end
    return state
end
function NS.ProjectSelection()
    local s, d = current(), db()
    local p = s.byProject[d.project_id]
    local t = p and s.byThread[p.id] and s.byThread[p.id][d.thread_id]
    d.project_id, d.thread_id = p and p.id or nil, t and t.id or nil
    return d.project_id, d.thread_id
end
function NS.SelectProject(projectID, threadID)
    local s = current()
    if not s.byProject[projectID] or (threadID and not (s.byThread[projectID] and s.byThread[projectID][threadID])) then return false end
    db().project_id, db().thread_id = projectID, threadID
    if panel then panel.refresh() end
    return true
end
function NS.ImportProjects(value)
    local parsed, err = NS.ParseProjects(value)
    if not parsed then NS.Notify("error", err) return nil, err end
    state, db().projectSnapshot = parsed, value
    NS.ProjectSelection()
    if panel then panel.refresh() end
    NS.Notify("success", "Project snapshot received. Select a project and chat.")
    return true
end
function NS.ProjectAction(action, name)
    local p, t = NS.ProjectSelection()
    if action ~= "create_project" and action ~= "create_chat" and action ~= "rename_project" and action ~= "rename_chat" then return false end
    name = type(name) == "string" and name:match("^%s*(.-)%s*$") or ""
    if name == "" or #name > 512 or name:find("[%z\1-\31\127]") then NS.Notify("error", "Enter a name (1–512 bytes).") return false end
    if action ~= "create_project" and not p then NS.Notify("error", "Select a project first.") return false end
    if action == "rename_chat" and not t then NS.Notify("error", "Select a chat first.") return false end
    if not NS.QueueRequest then NS.Notify("error", "Transport unavailable; request not queued.") return false end
    return NS.QueueRequest({schemaVersion=1, source="nanocodex-wow", type="nanocodex.action", action=action,
        project_id=action ~= "create_project" and p or nil, thread_id=action == "rename_chat" and t or nil, name=name})
end
local function safe(s) return s:gsub("|", "||") end
function NS.Projects()
    if not panel then
        panel = NS.BridgeSurface("NanocodexWowProjects", 700, 590)
        NS.BridgeText(panel, "Nanocodex · projects and chats", "GameFontNormalLarge", 16, -18, 640)
        NS.BridgeText(panel, "Projects and chats update automatically when the companion sends a snapshot.", "GameFontHighlightSmall", 16, -48, 650)
        local function button(label, x, y, width, callback)
            local b = CreateFrame("Button", nil, panel, "UIPanelButtonTemplate")
            b:SetSize(width, 24) b:SetPoint("TOPLEFT", x, y) b:SetText(label) b:SetScript("OnClick", callback)
            return b
        end
        local status = NS.BridgeText(panel, "Disconnected · transport unavailable", "GameFontHighlightSmall", 18, -82, 640)
        panel:SetScript("OnUpdate", function(self, elapsed)
            self.statusElapsed = (self.statusElapsed or 0) + elapsed
            if self.statusElapsed < 0.25 then return end
            self.statusElapsed = 0
            if NS.TransportDisplay then status:SetText(NS.TransportDisplay()) end
        end)
        local selected = NS.BridgeText(panel, "", "GameFontHighlightSmall", 18, -171, 640)
        local rows = {}
        local scroll = CreateFrame("ScrollFrame", nil, panel, "UIPanelScrollFrameTemplate")
        scroll:SetPoint("TOPLEFT", 18, -201) scroll:SetSize(642, 252)
        local child = CreateFrame("Frame", nil, scroll) child:SetSize(635, 252) scroll:SetScrollChild(child)
        panel.refresh = function()
            local p, t = NS.ProjectSelection()
            local s, entries = current(), {}
            for _, project in ipairs(s.projects) do
                entries[#entries+1] = {p=project.id, label=(project.id == p and "> " or "") .. project.name}
                if project.id == p then
                    for _, thread in ipairs(s.threads) do
                        if thread.project_id == p then
                            entries[#entries+1] = {p=p, t=thread.id, label="    " .. (thread.id == t and "> " or "") .. thread.title .. " [" .. thread.status .. "]"}
                        end
                    end
                end
            end
            selected:SetText(p and ("Selected: " .. safe(s.byProject[p].name) .. (t and " / " .. safe(s.byThread[p][t].title) or " / select a chat")) or "No selection · waiting for project snapshot")
            for i, entry in ipairs(entries) do
                local row = rows[i]
                if not row then
                    row = CreateFrame("Button", nil, child, "UIPanelButtonTemplate")
                    row:SetSize(625, 24) row:SetPoint("TOPLEFT", 0, -(i-1)*26) rows[i] = row
                    row:SetScript("OnClick", function(self) NS.SelectProject(self.projectID, self.threadID) end)
                end
                row.projectID, row.threadID = entry.p, entry.t
                row:SetText(safe(entry.label)) row:Show()
            end
            for i=#entries+1,#rows do rows[i]:Hide() end
            child:SetHeight(math.max(252, #entries*26))
            scroll:SetVerticalScroll(0) scroll:UpdateScrollChildRect()
        end
        NS.BridgeText(panel, "Name for create / rename request", "GameFontHighlightSmall", 18, -468, 630)
        local name = CreateFrame("EditBox", nil, panel, "InputBoxTemplate")
        name:SetSize(635, 26) name:SetPoint("TOPLEFT", 24, -488) name:SetAutoFocus(false)
        name:SetFontObject(ChatFontNormal or "GameFontHighlight") name:SetMaxLetters(512)
        name:SetScript("OnEscapePressed", function(self) self:ClearFocus() end)
        for i, spec in ipairs({{"Create project", "create_project"}, {"New chat", "create_chat"}, {"Rename project", "rename_project"}, {"Rename chat", "rename_chat"}}) do
            local action = spec[2]
            button(spec[1], 16+(i-1)*166, -535, 158, function() NS.ProjectAction(action, name:GetText()) name:ClearFocus() end)
        end
        panel.nameInput, panel.rows = name, rows
        panel:SetScript("OnHide", function() name:ClearFocus() end)
        table.insert(UISpecialFrames, "NanocodexWowProjects")
    end
    panel.refresh() panel:Show()
end
