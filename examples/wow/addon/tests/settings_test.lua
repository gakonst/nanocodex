-- Run from repo root: lua addon/tests/settings_test.lua
-- Stateful API doubles verify effects and rollback, not just success messages.
local function fixture(legacy)
    local f = {bindings={M="OPENALLBAGS"}, cvars={useUiScale="0", uiScale="0.9", nameplateShowEnemies="0"}, writes={}, notices={}, set=2, combat=false}
    local NS = {Notify=function(kind, text) f.notices[#f.notices+1] = {kind=kind, text=text} end}
    CreateFrame = function()
        return {RegisterEvent=function(_, event) assert(event == "PLAYER_REGEN_ENABLED") end,
            SetScript=function(_, event, callback) assert(event == "OnEvent") f.event=callback end}
    end
    InCombatLockdown = function() return f.combat end
    GetCurrentBindingSet = function() return f.set end
    GetBindingAction = function(k) return f.bindings[k] or "" end
    SetBinding = function(k, v)
        f.writes[#f.writes+1] = {"binding", k, v}
        if f.rejectBinding then return false end
        f.bindings[k] = v
        return true
    end
    SaveBindings = function(set)
        assert(set == f.set)
        f.writes[#f.writes+1] = {"save", set}
        if f.failSave then f.failSave=false return false end
    end
    GetCVar = function(k) return f.cvars[k] end
    SetCVar = function(k, v)
        f.writes[#f.writes+1] = {"cvar", k, v}
        if f.failCVar == k then f.failCVar=nil error("simulated CVar failure") end
        if f.ignoreCVar == k then f.ignoreCVar=nil return end
        f.cvars[k] = v
    end
    C_CVar = legacy and nil or {GetCVar=GetCVar, SetCVar=SetCVar}
    issecretvalue = nil
    assert(loadfile("addon/Nanocodex/Settings.lua"))("Nanocodex", NS)
    f.command = NS.Settings
    function f:notice(kind, fragment)
        local n = self.notices[#self.notices]
        assert(n and n.kind == kind and n.text:find(fragment, 1, true), n and n.text or "missing notice")
    end
    return f
end

local f = fixture()
for _, command in ipairs({"bind shift-ctrl-m TOGGLEWORLDMAP", "unbind M", "hud scale 0.7", "hud nameplates on"}) do
    f.command("preview " .. command)
    f:notice("status", "Preview:")
end
assert(#f.writes == 0 and f.bindings.M == "OPENALLBAGS" and f.cvars.uiScale == "0.9")
f.combat=true
f.command("preview bind M TOGGLEWORLDMAP")
f.combat=false
f.event()
assert(#f.writes == 0, "preview must never queue a change")
f.command("status")
f:notice("status", "Pending: none; undo: none")

f.command("  apply   bind shift-ctrl-m toggleworldmap  ")
assert(f.bindings["CTRL-SHIFT-M"] == "TOGGLEWORLDMAP")
assert(f.writes[2][1] == "save" and f.writes[2][2] == 2)
f:notice("success", "Applied:")
f.command("undo")
assert(f.bindings["CTRL-SHIFT-M"] == nil)
f:notice("success", "Undone:")
f.command("undo")
f:notice("error", "No change to undo")
f.command("apply unbind M")
assert(f.bindings.M == nil)
f.set=1
local count = #f.writes
f.command("undo")
f:notice("error", "another binding set")
assert(#f.writes == count)
f.set=2
f.command("undo")
assert(f.bindings.M == "OPENALLBAGS")

for _, legacy in ipairs({false, true}) do
    f=fixture(legacy)
    f.command("apply hud scale 0.64")
    assert(f.cvars.useUiScale == "1" and f.cvars.uiScale == "0.64")
    assert(f.writes[1][2] == "useUiScale" and f.writes[2][2] == "uiScale")
    f.command("preview hud nameplates on")
    f.command("undo")
    assert(f.cvars.useUiScale == "0" and f.cvars.uiScale == "0.9", "preview must preserve undo")
    f.command("apply hud nameplates on")
    assert(f.cvars.nameplateShowEnemies == "1")
    f.command("undo")
    assert(f.cvars.nameplateShowEnemies == "0")
end

f=fixture()
f.combat=true
f.command("apply bind M TOGGLEWORLDMAP")
f:notice("status", "Queued until combat ends")
assert(#f.writes == 0)
f.command("apply hud scale 0.8")
f:notice("error", "A change is pending")
f.command("undo")
f:notice("error", "Cancel pending change")
f.event()
assert(#f.writes == 0)
f.combat=false
f.event()
assert(f.bindings.M == "TOGGLEWORLDMAP" and #f.writes == 2)
f.event()
assert(#f.writes == 2, "queued changes run once")
f.combat=true
f.command("undo")
f:notice("error", "leaving combat")
assert(#f.writes == 2)
f.combat=false
f.command("undo")
assert(f.bindings.M == "OPENALLBAGS")
f.combat=true
f.command("apply hud nameplates on")
f.command("cancel")
f.combat=false
count=#f.writes
f.event()
assert(#f.writes == count and f.cvars.nameplateShowEnemies == "0")

f=fixture()
for _, bad in ipairs({"apply bind CTRL-CTRL-M TOGGLEWORLDMAP", "apply bind META-M TOGGLEWORLDMAP", "apply bind M OTHER", "apply bind CTRL--M TOGGLEWORLDMAP", "apply unbind F25", "apply hud scale 0.63", "apply hud scale 1.01", "apply hud scale 0..8", "apply hud nameplates maybe", string.rep("x",201)}) do
    f.command(bad)
    f:notice("error", "Error:")
end
assert(#f.writes == 0)
f.command("apply hud nameplates on")
f.failCVar="uiScale"
f.command("apply hud scale 0.8")
f:notice("error", "previous value restored")
assert(f.cvars.uiScale == "0.9" and f.cvars.useUiScale == "0")
f.command("undo")
assert(f.cvars.nameplateShowEnemies == "0", "failed apply must preserve previous undo")
f.ignoreCVar="uiScale"
f.command("apply hud scale 0.8")
f:notice("error", "CVar verification failed")
assert(f.cvars.useUiScale == "0" and f.cvars.uiScale == "0.9")
f.failSave=true
f.command("apply bind M TOGGLEWORLDMAP")
f:notice("error", "SaveBindings failed; previous value restored")
assert(f.bindings.M == "OPENALLBAGS")
f.rejectBinding=true
f.command("apply bind M TOGGLEWORLDMAP")
f:notice("error", "ROLLBACK FAILED")
f.rejectBinding=false
f.combat=true
f.command("apply hud scale 0.8")
f.failCVar="uiScale"
f.combat=false
f.event()
f:notice("error", "Queued change failed:")
assert(f.cvars.useUiScale == "0" and f.cvars.uiScale == "0.9")
count=#f.writes
f.event()
assert(#f.writes == count, "failed queue must not retry implicitly")

f=fixture()
GetBindingAction=nil
f.command("apply bind M TOGGLEWORLDMAP")
f:notice("error", "Required API unavailable")
assert(#f.writes == 0)
f=fixture()
local secret={}
issecretvalue=function(v) return v == secret end
C_CVar.GetCVar=function() return secret end
f.command("apply hud scale 0.8")
f:notice("error", "Restricted API value")
assert(#f.writes == 0)
print("PASS: settings preview isolation, binding/CVar apply and undo, modern/legacy APIs, combat queue/cancel, validation, rollback and API errors")
