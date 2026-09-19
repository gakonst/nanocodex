-- Run from repo root: lua addon/tests/context_test.lua
local NS = {}
assert(loadfile("addon/Nanocodex/Context.lua"))("NanocodexWow", NS)
local empty = NS.Capture()
assert(NS.Encode(empty):find('"quests":%[%]'))
assert(empty.schemaVersion == 1 and empty.target.exists == nil)
assert(NS.Encode({text='a"b\\c\n\t\0'}) == '{"text":"a\\"b\\\\c\\n\\t\\u0000"}')

UnitName = function(unit) return unit == "player" and "Testér" or "Target", "Realm" end
UnitClass = function() return "Mage", "MAGE", 8 end
UnitLevel = function() return 80 end
UnitExists = function() return true end
GetServerTime = function() return 1750000000 end
GetZoneText = function() return "Zone" end
GetBuildInfo = function() return "12.0.0", "12345", "date", 120000 end
C_SpecializationInfo = {
    GetSpecialization = function() return 1 end,
    GetSpecializationInfo = function(i) assert(i == 1) return 62, "Arcane" end,
}
C_QuestLog = {
    GetNumQuestLogEntries = function() return 3 end,
    GetInfo = function(i)
        if i == 1 then return { title="Header", isHeader=true } end
        return { title='Quest "' .. i .. '"', questID=100+i, level=80, isHeader=false }
    end,
    IsComplete = function(id) return id == 102 end,
}
local modern = NS.Capture()
assert(modern.specialization.id == 62 and #modern.quests == 2)
assert(modern.quests[1].complete == true and modern.quests[2].complete == false)
assert(modern.character.name == "Testér" and modern.client.interface == 120000)

-- Simulate values which must never reach comparisons or serialization.
local secret = setmetatable({}, {__tostring=function() error("secret leaked") end})
issecretvalue = function(v) return rawequal(v, secret) end
UnitName = function() return secret end
C_QuestLog.GetInfo = function(i)
    if i == 1 then return secret end
    return {title=secret, isHeader=false, questID=secret}
end
assert(NS.Capture().character.name == nil)
assert(#NS.Capture().quests == 0)
UnitLevel = function() error("restricted") end
assert(NS.Capture().character.level == nil)
UnitLevel = function() return math.huge end
assert(NS.Capture().character.level == nil)

-- Classic fallbacks preserve quest ID's eighth tuple position and talent-tree semantics.
C_SpecializationInfo, C_QuestLog = nil, nil
GetNumTalentTabs = function() return 3 end
GetTalentTabInfo = function(i) return "Tree " .. i, "icon", i == 2 and 31 or 0 end
GetNumQuestLogEntries = function() return 2 end
GetQuestLogTitle = function(i)
    if i == 1 then return "Heading", 1, nil, true end
    return "Classic Quest", 20, nil, false, false, 1, nil, 999
end
local classic = NS.Capture()
assert(classic.specialization.talentTree == 2 and classic.specialization.id == nil)
assert(#classic.quests == 1 and classic.quests[1].id == 999)
assert(classic.quests[1].complete == 1)
print("PASS: absent APIs, modern API capture, JSON escaping, restricted/error values, Classic fallbacks")
