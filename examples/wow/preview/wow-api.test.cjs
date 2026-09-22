// Run: node examples/wow/preview/wow-api.test.cjs
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { lua, lauxlib, lualib, to_luastring, to_jsstring } = require('fengari');
const L = lauxlib.luaL_newstate();
lualib.luaL_openlibs(L);
function run(source) {
  if (lauxlib.luaL_dostring(L, to_luastring(source)) !== lua.LUA_OK) {
    throw new Error(to_jsstring(lua.lua_tostring(L, -1)));
  }
}
function call(name, ...args) {
  lua.lua_getglobal(L, to_luastring(name));
  for (const arg of args) {
    if (arg == null) lua.lua_pushnil(L);
    else if (typeof arg === 'boolean') lua.lua_pushboolean(L, arg);
    else if (typeof arg === 'number') lua.lua_pushnumber(L, arg);
    else lua.lua_pushstring(L, to_luastring(arg));
  }
  if (lua.lua_pcall(L, args.length, 1, 0) !== lua.LUA_OK) {
    throw new Error(to_jsstring(lua.lua_tostring(L, -1)));
  }
  const value = lua.lua_type(L, -1) === lua.LUA_TSTRING ? to_jsstring(lua.lua_tostring(L, -1)) : null;
  lua.lua_pop(L, 1);
  return value;
}
const snapshot = () => JSON.parse(call('PreviewSnapshot'));
run(fs.readFileSync(path.join(__dirname, 'wow-api.lua'), 'utf8'));
run('assert(next(NS)==nil)');
for (const name of ['Context', 'Core', 'Projects', 'Bridge', 'Client']) {
  call('PreviewLoad', fs.readFileSync(path.join(__dirname, '../addon/Nanocodex', name + '.lua'), 'utf8'));
}
run('assert(NanocodexWowDB == nil)');
call('PreviewRestore', 'Saved draft', true, true, 20, 30);
call('PreviewStart');
let view = snapshot();
assert(Array.isArray(view.frames) && Array.isArray(view.requests) && Array.isArray(view.notices));
let panel = view.frames.find(f => f.name === 'NanocodexWowPanel');
let prompt = view.frames.find(f => f.name === 'NanocodexWowPrompt');
assert.equal(panel.visible, false);
assert.equal(prompt.text, 'Saved draft');
assert.equal(panel.points[0].x, 20);
assert.equal(panel.points[0].y, -30);
assert.equal(view.saved.minimized, true);
assert(view.frames.some(f => f.kind === 'ScrollFrame' && f.points.length === 2));
assert(view.frames.every(f => Array.isArray(f.points)));
run(`
assert(not pcall(function() UIParent:UnsupportedMethod() end))
local f=CreateFrame('Frame', nil, UIParent)
f:SetPoint('CENTER')
f:SetPoint('TOPLEFT', 2, -3)
f:SetPoint('BOTTOMRIGHT', UIParent, -4, 5)
assert(#f.points==3 and f.points[2].relative==UIParent)
f:SetPoint('TOPLEFT', 'UIParent', 'TOPLEFT', 8, -9)
assert(#f.points==3 and f.points[2].x==8)
assert(UnitName==nil and UnitLevel==nil and UnitExists==nil)
`);
call('PreviewCommand', 'show');
view = snapshot();
const expand = view.frames.find(f => f.text === 'Expand');
call('PreviewEvent', expand.id, 'click');
call('PreviewEvent', prompt.id, 'input', 'hello browser');
call('PreviewEvent', prompt.id, 'focus');
call('PreviewEvent', prompt.id, 'enter');
view = snapshot();
assert.equal(view.requests.length, 0);
assert.equal(view.saved.draft, 'hello browser');
assert(view.notices.some(n => n.includes('Connect Nanocodex first')));
call('PreviewLink', true, 305419896);
call('PreviewEvent', prompt.id, 'enter');
view = snapshot();
assert.equal(view.requests.length, 1);
assert.equal(view.requests[0].id, 'ncw:12345678:0001');
let payload = JSON.parse(view.requests[0].payload);
assert.equal(payload.prompt, 'hello browser');
assert.deepEqual(payload.context.character, {});
assert.deepEqual(payload.context.quests, []);
assert.equal(payload.context.location.zone, 'Game context unavailable outside WoW');
assert.equal(view.saved.draft, '');
run('assert(not NS.TransportStatus().pending and NS.TransportStatus().inflight==1 and NS.TransportStatus().message_id==1)');
call('PreviewCommand', 'ask hello browser');
assert.equal(snapshot().requests.length, 0, 'duplicate pending prompt must not be admitted');
run('assert(NS.TransportStatus().inflight==1 and NS.TransportStatus().message_id==1)');
call('PreviewCommand', 'ask independent');
view = snapshot();
assert.equal(view.requests.length, 1);
assert.equal(view.requests[0].id, 'ncw:12345678:0002');
assert(!view.notices.some(n => n.includes('another request')));
run('assert(NS.TransportStatus().inflight==2)');
call('PreviewAccepted', 'ncw:12345678:0001');
call('PreviewAccepted', 'ncw:12345678:0001'); // Duplicate receipt cannot clear another request.
run('assert(NS.TransportStatus().inflight==1)');
call('PreviewAccepted', 'ncw:12345678:0002');
run('assert(NS.TransportStatus().inflight==0)');
call('PreviewReceive', 'transport_ack', '1');
call('PreviewReceive', 'projects', 'ncw1\nP\tp\tFixture%20Project\nT\tp\tt\tFixture%20Chat\tidle');
view = snapshot();
const thread = view.frames.find(f => f.kind === 'Button' && f.text.includes('Fixture Chat'));
call('PreviewEvent', thread.id, 'click');
view = snapshot();
payload = JSON.parse(view.requests[0].payload);
assert.equal(payload.action, 'load_history');
assert.equal(payload.thread_id, 't');
assert.equal(view.saved.thread_id, 't');
const activeRow = view.frames.find(f => f.questRow && f.threadID === 't');
assert.equal(activeRow.active, true);
assert(activeRow.text.startsWith('Current quest:'));
assert(view.frames.some(f => f.text === 'CURRENT QUEST' && f.visible));
run(`
assert(NanocodexWowPanel:GetWidth() == 820)
local _, _, _, trackerX = NanocodexWowPanel.threadScroll:GetPoint()
local _, _, _, titleX = NanocodexWowPanel.conversationTitle:GetPoint()
assert(trackerX > titleX + NanocodexWowPanel.conversationTitle:GetWidth())
assert(NanocodexWowPanel.threadRows[1].active)
assert(NanocodexWowPanel.threadRows[1].detail:GetText():find('CURRENT', 1, true))
`);

call('PreviewAccepted');
call('PreviewCommand', 'hide');
call('PreviewReceive', 'reply', `nch1\thistory\tt\t0\t1\t\t0\t${payload.view_id}\nAssistant\nFixture history`);
call('PreviewTick', .05);
view = snapshot();
assert(view.frames.some(f => f.text === 'Assistant\nFixture history' && !f.visible));
assert.equal(view.saved.hidden, true);
call('PreviewCommand', 'show');
for (const [label, action] of [['New chat', 'create_chat'], ['Reconnect', 'reconnect'], ['Refresh', 'refresh_projects']]) {
  view = snapshot();
  const button = view.frames.find(f => f.kind === 'Button' && f.text === label);
  call('PreviewEvent', button.id, 'click');
  view = snapshot();
  assert.equal(JSON.parse(view.requests[0].payload).action, action);
  call('PreviewAccepted');
}
call('PreviewCommand', 'hide');
call('PreviewTick', .05);
view = snapshot();
assert.equal(JSON.parse(view.requests[0].payload).action, 'connection_status');
call('PreviewAccepted');
call('PreviewResize', 400, 560);
call('PreviewMove', panel.id, 35, 45);
view = snapshot();
panel = view.frames.find(f => f.name === 'NanocodexWowPanel');
assert(panel.scale * 700 <= 536 + 1e-8 && panel.scale * 820 <= 376 + 1e-8);
assert.equal(view.saved.position.x, 35);
assert.equal(view.saved.position.y, -45);
call('PreviewCommand', 'projects');
view = snapshot();
assert(view.frames.some(f => f.name === 'NanocodexWowProjects' && f.visible));
run(`
local f=CreateFrame('Frame')
f:SetScript('OnUpdate', function(self) self.ticks=(self.ticks or 0)+1 end)
PreviewTick(.05) assert(f.ticks==1)
f:Hide() PreviewTick(.05) assert(f.ticks==1)
local child=CreateFrame('Frame', nil, f)
child:SetScript('OnUpdate', function() error('Hidden descendant updated') end)
PreviewTick(.05)
`);
console.log('PASS: Fengari real addon lifecycle, restore/snapshot arrays, strict APIs, anchors, callbacks, disconnected/pending transport, identity, thread clicks/history, hidden pump, resize/drag, projects');
