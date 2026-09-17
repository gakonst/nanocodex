-- Run from this directory: lua test_mock.lua > mock-export.json
local secretToken = setmetatable({}, { __tostring = function() error('secret converted') end })
function issecretvalue(v) return rawequal(v, secretToken) end
local secretChecks, editReads, ticks = 0, 0, 0
local originalSecret = issecretvalue
function issecretvalue(v) if rawequal(v, secretToken) then secretChecks = secretChecks + 1 end; return originalSecret(v) end
function GetBuildInfo() return 'mock', '123', 'mock date', 120001 end
function GetServerTime() return 1000 end
function GetTime() return 10 end
function debugprofilestop() ticks = ticks + 0.01; return ticks end
local methods = {}
function methods:GetObjectType() return self.role or 'Frame' end
function methods:IsVisible() return self.visible ~= false end
function methods:GetName() return self.name end
function methods:GetLeft() return 10 end
function methods:GetBottom() return 20 end
function methods:GetWidth() return 100 end
function methods:GetHeight() return 50 end
function methods:GetEffectiveScale() return 0.8 end
function methods:GetText() if self.role == 'EditBox' then editReads = editReads + 1; error('EditBox read') end; return self.text end
function methods:GetRegions() return nil end
function methods:SetText(text) self.text = text end
function methods:SetScript(event, fn) self[event] = fn end
function methods:Hide() self.visible = false; if self.OnHide then self:OnHide() end end
function methods:Show() self.visible = true end
local function obj(t) return setmetatable(t or {}, { __index = function(_, k) return methods[k] or function() end end }) end
function methods:CreateFontString() return obj() end
UIParent = obj()
function CreateFrame(_, name) local o = obj({ name = name }); if name then _G[name] = o end; return o end
UISpecialFrames, SlashCmdList = {}, {}
local frames = { obj({ name = 'Button', role = 'Button', text = 'quote" slash\\ newline\n\1' }),
    obj({ name = 'Password', role = 'EditBox', text = 'MUST_NOT_EXPORT' }),
    obj({ name = secretToken, role = 'Button', text = secretToken }) }
function EnumerateFrames(previous)
    if not previous then return frames[1] end
    for i, f in ipairs(frames) do if f == previous then return frames[i + 1] end end
end
local hooks = {}
function hooksecurefunc(name, fn) hooks[name] = fn end
function bsspeak() end
assert(loadfile('NanocodexObserve.lua'))()
assert(hooks.bsspeak)
for i = 1, 45 do hooks.bsspeak('speech ' .. i) end
hooks.bsspeak(secretToken)
SlashCmdList.NANOCODEXOBSERVE('')
assert(type(NanocodexObserveSnapshot) == 'string')
assert(editReads == 0 and secretChecks >= 3)
assert(not NanocodexObserveSnapshot:find('MUST_NOT_EXPORT', 1, true))
assert(not NanocodexObserveSnapshot:find('speech 5"', 1, true))
assert(NanocodexObserveSnapshot:find('speech 6"', 1, true))
assert(NanocodexObserveSnapshot:find('"sequence":1', 1, true))
local first = NanocodexObserveSnapshot
SlashCmdList.NANOCODEXOBSERVE('close')
assert(NanocodexObserveExport.visible == false)
-- Continuous enumeration must terminate at an explicit budget.
function EnumerateFrames() return obj({ role = 'Frame' }) end
SlashCmdList.NANOCODEXOBSERVE('')
assert(NanocodexObserveSnapshot:find('"truncated":true', 1, true))
assert(NanocodexObserveSnapshot:find('"sequence":2', 1, true))
-- Timer budget stops a scan independently of frame counts.
function debugprofilestop() ticks = ticks + 30; return ticks end
SlashCmdList.NANOCODEXOBSERVE('')
assert(NanocodexObserveSnapshot:find('"reason":"time_budget"', 1, true))
-- Missing optional dependency and profiling clock do not prevent export.
bsspeak, hooksecurefunc, debugprofilestop = nil, nil, nil
function EnumerateFrames() return nil end
SlashCmdList.NANOCODEXOBSERVE('')
assert(NanocodexObserveSnapshot:find('"frames":[]', 1, true))
assert(NanocodexObserveSnapshot:find('"timer_available":false', 1, true))
io.write(first)
