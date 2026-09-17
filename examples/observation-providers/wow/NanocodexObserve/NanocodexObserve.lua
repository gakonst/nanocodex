-- Original standalone prototype. No game actions, external transport, or polling.
local MAX_VISITED, MAX_FRAMES, MAX_REGIONS = 1500, 250, 8
local MAX_MS, MAX_TEXT, MAX_SPEECH = 25, 512, 40
local sequence, speech, hooked = 0, {}, {}
local panel, edit
local textTruncations = 0
local function secret(v)
    if issecretvalue then
        local ok, result = pcall(issecretvalue, v)
        if not ok or result then return true end
    end
    return false
end
local function primitive(v)
    if secret(v) then return nil end
    local t = type(v)
    if t == 'string' then
        -- Preserve UTF-8 boundaries when clipping labels.
        if #v > MAX_TEXT then
            textTruncations = textTruncations + 1
            local n = MAX_TEXT
            while n > 0 do
                local b = string.byte(v, n + 1)
                if not b or b < 128 or b >= 192 then break end
                n = n - 1
            end
            return string.sub(v, 1, n)
        end
        return v
    end
    if t == 'number' and v == v and v ~= math.huge and v ~= -math.huge then return v end
    if t == 'boolean' then return v end
end
local function call(object, method, ...)
    if secret(object) then return nil end
    local ok, value = pcall(function(...) return object[method](object, ...) end, ...)
    if not ok or secret(value) then return nil end
    return value
end
local function value(object, method) return primitive(call(object, method)) end
local function global(name)
    local f = _G[name]
    if secret(f) or type(f) ~= 'function' then return nil end
    local ok, v = pcall(f)
    if ok then return primitive(v) end
end
local function quote(s)
    return '"' .. s:gsub('[%z\1-\31\\"]', function(c)
        return string.format('\\u%04x', string.byte(c))
    end) .. '"'
end
local function json(v)
    if secret(v) then return 'null' end
    local t = type(v)
    if t == 'string' then return quote(v) end
    if t == 'number' then return primitive(v) and tostring(v) or 'null' end
    if t == 'boolean' then return v and 'true' or 'false' end
    if t ~= 'table' then return 'null' end
    local parts = {}
    if v.__array then
        for i = 1, #v do parts[#parts + 1] = json(v[i]) end
        return '[' .. table.concat(parts, ',') .. ']'
    end
    local keys = {}
    for k in pairs(v) do if not secret(k) and type(k) == 'string' then keys[#keys + 1] = k end end
    table.sort(keys)
    for _, k in ipairs(keys) do parts[#parts + 1] = quote(k) .. ':' .. json(v[k]) end
    return '{' .. table.concat(parts, ',') .. '}'
end
local function array() return { __array = true } end
local function stamp() return { server_seconds = global('GetServerTime'), uptime_seconds = global('GetTime') } end
local function installHooks()
    if not hooksecurefunc then return end
    for _, name in ipairs({ 'bsspeak', 'bsspeak2' }) do
        local fn = _G[name]
        if not secret(fn) and type(fn) == 'function' and not hooked[name] then
            local ok = pcall(hooksecurefunc, name, function(...)
                -- Hook cannot interfere with the original function, including on restricted values.
                pcall(function(...)
                    local before = textTruncations
                    local text = primitive(select(1, ...))
                    if type(text) ~= 'string' then return end
                    speech[#speech + 1] = { source = name, text = text, captured = stamp(), truncated = textTruncations > before }
                    if #speech > MAX_SPEECH then table.remove(speech, 1) end
                end, ...)
            end)
            if ok then hooked[name] = true end
        end
    end
end
local function buildInfo()
    local ok, version, build, date, interface = pcall(GetBuildInfo)
    if not ok then return {} end
    return { version = primitive(version), build = primitive(build), date = primitive(date), interface = primitive(interface) }
end
local function scan()
    local frames, visited, reason = array(), 0, nil
    local initialTextTruncations, regionsTruncated = textTruncations, false
    local start = global('debugprofilestop')
    local function expired()
        local now = global('debugprofilestop')
        return type(start) == 'number' and type(now) == 'number' and now - start >= MAX_MS
    end
    local cursor
    while visited < MAX_VISITED and #frames < MAX_FRAMES do
        if expired() then reason = 'time_budget'; break end
        local ok, frame = pcall(EnumerateFrames, cursor)
        if not ok then reason = 'enumeration_error'; break end
        if secret(frame) then reason = 'secret_frame'; break end
        if frame == nil then break end
        cursor = frame
        visited = visited + 1
        if frame ~= panel and frame ~= edit and value(frame, 'IsVisible') == true then
            local role = value(frame, 'GetObjectType')
            local row = { name = value(frame, 'GetName'), role = role,
                source = 'EnumerateFrames', labels = array(),
                rect = { left = value(frame, 'GetLeft'), bottom = value(frame, 'GetBottom'),
                    width = value(frame, 'GetWidth'), height = value(frame, 'GetHeight'),
                    effective_scale = value(frame, 'GetEffectiveScale') } }
            -- Unknown roles are also conservatively redacted. Never call GetText on EditBox.
            if type(role) == 'string' and role ~= 'EditBox' then
                local text = value(frame, 'GetText')
                if type(text) == 'string' then row.labels[#row.labels + 1] = text end
                -- select bounds the inspected regions (GetRegions itself is a single API call).
                for i = 1, MAX_REGIONS do
                    if expired() then reason = 'time_budget'; break end
                    local rok, region = pcall(function() return select(i, frame:GetRegions()) end)
                    if not rok or secret(region) then break end
                    if region == nil then break end
                    if value(region, 'GetObjectType') == 'FontString' and value(region, 'IsVisible') == true then
                        local label = value(region, 'GetText')
                        if type(label) == 'string' then row.labels[#row.labels + 1] = label end
                    end
                end
                local extraOk, extra = pcall(function() return select(MAX_REGIONS + 1, frame:GetRegions()) end)
                if extraOk and not secret(extra) and extra ~= nil then regionsTruncated = true end
            else row.text_redacted = true end
            if #row.labels > 0 or (row.name and (role == 'Button' or role == 'CheckButton' or role == 'EditBox' or role == 'Slider')) then
                frames[#frames + 1] = row
            end
        end
        if reason then break end
    end
    if not reason and (visited >= MAX_VISITED or #frames >= MAX_FRAMES) then reason = 'count_budget' end
    return frames, { visited = visited, emitted = #frames, truncated = reason ~= nil or textTruncations > initialTextTruncations or regionsTruncated, reason = reason,
        text_truncated = textTruncations > initialTextTruncations, regions_truncated = regionsTruncated,
        max_visited = MAX_VISITED, max_frames = MAX_FRAMES, max_regions_per_frame = MAX_REGIONS,
        max_text_bytes = MAX_TEXT, max_ms = MAX_MS, timer_available = type(start) == 'number' }
end
local function capture()
    sequence = sequence + 1
    local started = stamp()
    local frames, limits = scan()
    local recent = array()
    for i, entry in ipairs(speech) do recent[i] = entry end
    return json({ schema = 'nanocodex.observe.v1', addon_version = '0.1.0', sequence = sequence,
        provenance = { source = 'WoW client Lua UI APIs; optional BlindSlash speech hooks',
            requested_by = '/ncobserve', started = started, finished = stamp() },
        build = buildInfo(), frames = frames, scan = limits, recent_speech = recent,
        speech_limit = MAX_SPEECH, speech_history_may_be_truncated = #speech >= MAX_SPEECH,
        coordinates = { basis = 'screen-relative coordinates in the frame UI scale, origin bottom-left; multiply rect coordinates by effective_scale for screen pixels',
            ui_parent_scale = value(UIParent, 'GetEffectiveScale'),
            ui_parent_width = value(UIParent, 'GetWidth'), ui_parent_height = value(UIParent, 'GetHeight') } })
end
local function show(text)
    if not panel then
        panel = CreateFrame('Frame', 'NanocodexObserveExport', UIParent, 'BackdropTemplate')
        panel:SetSize(700, 440); panel:SetPoint('CENTER'); panel:SetFrameStrata('DIALOG')
        panel:SetBackdrop({ bgFile = 'Interface/Tooltips/UI-Tooltip-Background', edgeFile = 'Interface/Tooltips/UI-Tooltip-Border', edgeSize = 16 })
        panel:SetBackdropColor(0, 0, 0, 1); panel:EnableMouse(true)
        local title = panel:CreateFontString(nil, 'OVERLAY', 'GameFontNormal')
        title:SetPoint('TOP', 0, -14); title:SetText('NanocodexObserve — Ctrl+A, Ctrl+C to copy; Escape to close')
        local scroll = CreateFrame('ScrollFrame', nil, panel, 'UIPanelScrollFrameTemplate')
        scroll:SetPoint('TOPLEFT', 18, -40); scroll:SetPoint('BOTTOMRIGHT', -38, 18)
        edit = CreateFrame('EditBox', nil, scroll)
        edit:SetMultiLine(true); edit:SetFontObject(ChatFontNormal); edit:SetWidth(630)
        edit:SetAutoFocus(false); edit:SetScript('OnEscapePressed', function() panel:Hide() end)
        scroll:SetScrollChild(edit)
        panel:SetScript('OnHide', function() edit:ClearFocus() end)
        UISpecialFrames[#UISpecialFrames + 1] = 'NanocodexObserveExport'
    end
    panel:Show(); edit:SetText(text); edit:SetFocus(); edit:HighlightText()
end
SLASH_NANOCODEXOBSERVE1 = '/ncobserve'
SlashCmdList.NANOCODEXOBSERVE = function(message)
    if primitive(message) == 'close' then if panel then panel:Hide() end; return end
    installHooks()
    if panel then panel:Hide() end -- Exclude the entire previous export subtree.
    local ok, text = pcall(capture)
    if ok then NanocodexObserveSnapshot = text; show(text)
    else print('NanocodexObserve: capture failed; no export produced.') end
end
local events = CreateFrame('Frame')
events:RegisterEvent('ADDON_LOADED')
events:SetScript('OnEvent', installHooks)
installHooks()
