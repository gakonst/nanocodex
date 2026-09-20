local _, NS = ...
NS = NS or {}
NanocodexWow = NS

-- Never compare, stringify or serialize restricted values returned by modern clients.
local function safe(value)
    if type(issecretvalue) == "function" and issecretvalue(value) then return nil end
    local kind = type(value)
    if kind == "string" or kind == "boolean" then return value end
    if kind == "number" and value == value and value ~= math.huge and value ~= -math.huge then return value end
end
local function call(fn, ...)
    if type(fn) ~= "function" then return nil end
    local ok, a, b, c, d, e, f, g, h = pcall(fn, ...)
    if not ok then return nil end
    return safe(a), safe(b), safe(c), safe(d), safe(e), safe(f), safe(g), safe(h)
end
local function quote(value)
    return '"' .. value:gsub('[%z\1-\31\\"]', function(c)
        local replacements = { ['"'] = '\\"', ['\\'] = '\\\\', ['\n'] = '\\n', ['\r'] = '\\r', ['\t'] = '\\t' }
        return replacements[c] or string.format('\\u%04x', string.byte(c))
    end) .. '"'
end
local function encode(value)
    local kind = type(value)
    if kind == "string" then return quote(value) end
    if kind == "number" or kind == "boolean" then return tostring(value) end
    if kind ~= "table" then return "null" end
    local parts = {}
    if getmetatable(value) == NS.array then
        for _, entry in ipairs(value) do parts[#parts + 1] = encode(entry) end
        return "[" .. table.concat(parts, ",") .. "]"
    end
    local keys = {}
    for key in pairs(value) do keys[#keys + 1] = key end
    table.sort(keys)
    for _, key in ipairs(keys) do parts[#parts + 1] = quote(key) .. ":" .. encode(value[key]) end
    return "{" .. table.concat(parts, ",") .. "}"
end
NS.array = {}
NS.Encode = encode
NS.SafeCall = call

function NS.Capture()
    local context = {
        schemaVersion = 1, source = "nanocodex-wow", addonVersion = "0.1.0",
        capturedAt = call(GetServerTime) or call(time),
        character = {}, specialization = {}, location = {}, target = {},
        quests = setmetatable({}, NS.array),
        limitations = "Manual snapshot; unavailable or restricted values omitted. Quest list may exclude collapsed headers. No live connection.",
    }
    local c = context.character
    c.name, c.realm = call(UnitName, "player")
    c.realm = c.realm or call(GetRealmName)
    c.class, c.classToken, c.classID = call(UnitClass, "player")
    c.level = call(UnitLevel, "player")
    c.faction = call(UnitFactionGroup, "player")
    local version, build, _, interface = call(GetBuildInfo)
    context.client = { version = version, build = build, interface = interface, projectID = safe(WOW_PROJECT_ID) }
    local specAPI = C_SpecializationInfo or {}
    local index = call(specAPI.GetSpecialization or GetSpecialization)
    if index then
        local id, name = call(specAPI.GetSpecializationInfo or GetSpecializationInfo, index)
        context.specialization = { index = index, id = id, name = name }
    elseif type(GetTalentTabInfo) == "function" then
        -- Classic: report the highest-investment talent tree, not a retail spec ID.
        local bestPoints = 0
        for i = 1, (call(GetNumTalentTabs) or 0) do
            local name, _, points = call(GetTalentTabInfo, i)
            if type(points) == "number" and points > bestPoints then
                bestPoints = points
                context.specialization = { name = name, talentTree = i, pointsSpent = points }
            end
        end
    end
    context.location.zone = call(GetZoneText)
    context.location.subzone = call(GetSubZoneText)
    context.location.mapID = call(C_Map and C_Map.GetBestMapForUnit, "player")
    local exists = call(UnitExists, "target")
    context.target.exists = exists
    if exists then
        context.target.name = call(UnitName, "target")
        context.target.level = call(UnitLevel, "target")
        context.target.isPlayer = call(UnitIsPlayer, "target")
        context.target.class, context.target.classToken = call(UnitClass, "target")
    end
    local modern = C_QuestLog and type(C_QuestLog.GetInfo) == "function"
    local count = call(modern and C_QuestLog.GetNumQuestLogEntries or GetNumQuestLogEntries) or 0
    -- Cap work on malformed APIs; never expand headers or modify the user's quest log.
    for i = 1, math.min(count, 500) do
        local title, level, header, complete, id
        if modern then
            local ok, info = pcall(C_QuestLog.GetInfo, i)
            if ok and not (type(issecretvalue) == "function" and issecretvalue(info)) and type(info) == "table" then
                title, level, header, id = safe(info.title), safe(info.level), safe(info.isHeader), safe(info.questID)
                if id then complete = call(C_QuestLog.IsComplete, id) end
            end
        else
            local tag, collapsed, frequency
            title, level, tag, header, collapsed, complete, frequency, id = call(GetQuestLogTitle, i)
        end
        if title and header == false then
            context.quests[#context.quests + 1] = { id = id, title = title, level = level, complete = complete }
        end
    end
    return context
end
