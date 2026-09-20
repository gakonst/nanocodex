local _, NS = ...
local pending, undo
local function say(s)
    if type(NS.Notify) == "function" then
        local kind = "status"
        if s:match("^Error:") or s:match("^Queued change failed:") then kind = "error"
        elseif s:match("^Applied:") or s:match("^Undone:") then kind = "success" end
        NS.Notify(kind, "Settings: " .. s)
    else
        print("|cffffd100Nanocodex settings:|r " .. s)
    end
end
local function call(fn, ...)
    if type(fn) ~= "function" then error("Required API unavailable", 0) end
    local ok, value = pcall(fn, ...)
    if not ok then error(tostring(value), 0) end
    if type(issecretvalue) == "function" and issecretvalue(value) then error("Restricted API value", 0) end
    return value
end
local bases = {}
for c in ("ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"):gmatch(".") do bases[c] = true end
for c in ("SPACE TAB ESCAPE BACKSPACE ENTER UP DOWN LEFT RIGHT HOME END INSERT DELETE PAGEUP PAGEDOWN"):gmatch("%S+") do bases[c] = true end
for i=1,24 do bases["F"..i] = true end
for i=1,5 do bases["BUTTON"..i] = true end
for i=0,9 do bases["NUMPAD"..i] = true end
local function key(s)
    s = s:upper()
    local parts = {}
    for p in s:gmatch("[^-]+") do parts[#parts+1] = p end
    if table.concat(parts, "-") ~= s or not bases[parts[#parts]] then error("Unsupported key", 0) end
    local mods = {}
    for i=1,#parts-1 do
        local p = parts[i]
        if (p ~= "CTRL" and p ~= "ALT" and p ~= "SHIFT") or mods[p] then error("Invalid modifiers", 0) end
        mods[p] = true
    end
    local result = ""
    for _,p in ipairs({"CTRL","ALT","SHIFT"}) do if mods[p] then result = result..p.."-" end end
    return result..parts[#parts]
end
local function parse(s)
    local k,a = s:match("^bind (%S+) (%S+)$")
    if k then if a:upper() ~= "TOGGLEWORLDMAP" then error("Unsupported action",0) end return {kind="binding", key=key(k), value=a:upper()} end
    k = s:match("^unbind (%S+)$")
    if k then return {kind="binding", key=key(k), value=""} end
    local v = s:match("^hud scale ([%d%.]+)$")
    if v then
        local n = tonumber(v)
        if not n or n < 0.64 or n > 1 then error("Scale must be 0.64 to 1.0",0) end
        return {kind="scale", values={useUiScale="1", uiScale=tostring(n)}}
    end
    v = s:match("^hud nameplates (%a+)$")
    if v == "on" or v == "off" then return {kind="nameplates", values={nameplateShowEnemies=v == "on" and "1" or "0"}} end
    error("Use preview/apply bind KEY TOGGLEWORLDMAP, unbind KEY, hud scale 0.64..1, hud nameplates on/off; undo, cancel, status",0)
end
local function getCVar(k)
    local v = call(C_CVar and C_CVar.GetCVar or GetCVar,k)
    if type(v) ~= "string" and type(v) ~= "number" then error("CVar unavailable: "..k,0) end
    return tostring(v)
end
local function snapshot(c)
    if c.kind == "binding" then
        local v = call(GetBindingAction,c.key)
        if type(v) ~= "string" then error("Binding unavailable",0) end
        local set = call(GetCurrentBindingSet)
        if set ~= 1 and set ~= 2 then error("Binding set unavailable",0) end
        return {kind=c.kind,key=c.key,value=v,set=set}
    end
    local r = {kind=c.kind,values={}}
    for k in pairs(c.values) do r.values[k] = getCVar(k) end
    return r
end
local function describe(c)
    if c.kind == "binding" then return c.key.."="..(c.value == "" and "unbound" or c.value) end
    if c.kind == "scale" then return "useUiScale="..c.values.useUiScale..", uiScale="..c.values.uiScale end
    return "nameplateShowEnemies="..c.values.nameplateShowEnemies
end
local function same(a,b) return a == b or (tonumber(a) and tonumber(a) == tonumber(b)) end
local function write(c, set)
    if c.kind == "binding" then
        if call(GetCurrentBindingSet) ~= set then error("Active binding set changed",0) end
        if not call(SetBinding,c.key,c.value ~= "" and c.value or nil) then error("SetBinding rejected change",0) end
        if call(GetBindingAction,c.key) ~= c.value then error("Binding verification failed",0) end
        if call(SaveBindings,set) == false then error("SaveBindings failed",0) end
        if call(GetBindingAction,c.key) ~= c.value then error("Saved binding verification failed",0) end
    else
        -- Enable custom scale before setting its value; restore both on failure.
        local names = c.kind == "scale" and {"useUiScale","uiScale"} or {"nameplateShowEnemies"}
        for _,k in ipairs(names) do
            if call(C_CVar and C_CVar.SetCVar or SetCVar,k,c.values[k]) == false then error("SetCVar rejected "..k,0) end
        end
        for _,k in ipairs(names) do if not same(getCVar(k),c.values[k]) then error("CVar verification failed: "..k,0) end end
    end
end
local function execute(c, isUndo)
    local before = snapshot(c)
    if isUndo and c.set and before.set ~= c.set then error("Undo belongs to another binding set",0) end
    local ok, err = pcall(write,c,before.set)
    if not ok then
        local restored, rollbackError = pcall(write,before,before.set)
        error(tostring(err)..(restored and "; previous value restored" or "; ROLLBACK FAILED: "..tostring(rollbackError)),0)
    end
    if isUndo then undo = nil else undo = before end
    say((isUndo and "Undone: " or "Applied: ")..describe(before).." -> "..describe(c))
end
local function combat() return call(InCombatLockdown) end
function NS.Settings(message)
    local ok, err = pcall(function()
        local s = (message or ""):gsub("^%s+",""):gsub("%s+$",""):gsub("%s+"," ")
        if #s > 200 then error("Command too long",0) end
        if s == "cancel" then pending=nil say("Pending change cancelled.") return end
        if s == "status" then say("Pending: "..(pending and describe(pending) or "none").."; undo: "..(undo and describe(undo) or "none")) return end
        if s == "undo" then
            if pending then error("Cancel pending change before undo",0) end
            if not undo then error("No change to undo in this session",0) end
            if combat() then error("Undo requires leaving combat",0) end
            execute(undo,true) return
        end
        local mode, body = s:match("^(%a+) (.+)$")
        if mode ~= "preview" and mode ~= "apply" then error("Use settings preview/apply, undo, cancel, or status",0) end
        local c = parse(body)
        local before = snapshot(c)
        if mode == "preview" then say("Preview: "..describe(before).." -> "..describe(c).." (no change queued)") return end
        if pending then error("A change is pending; cancel it first",0) end
        if combat() then pending=c say("Queued until combat ends: "..describe(before).." -> "..describe(c)..". /nc settings cancel to cancel.") return end
        execute(c,false)
    end)
    if not ok then say("Error: "..tostring(err)) end
end
local events = CreateFrame("Frame")
events:RegisterEvent("PLAYER_REGEN_ENABLED")
events:SetScript("OnEvent",function()
    if not pending then return end
    local ok, err = pcall(function()
        if combat() then return end
        local c=pending pending=nil execute(c,false)
    end)
    if not ok then say("Queued change failed: "..tostring(err)) end
end)
