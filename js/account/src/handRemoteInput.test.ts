import assert from "node:assert/strict";
import test from "node:test";
import { RemoteMotionBuffer, RemoteMouseButtons } from "./handRemoteInput.ts";
import type { RemoteInput } from "./handRemote.ts";

test("mouse chords track left/right/middle independently, including pointermove transitions", () => {
  const mouse = new RemoteMouseButtons(), events: RemoteInput[] = [];
  const send = (event: RemoteInput) => events.push(event);
  for (const buttons of [1, 3, 3, 2, 6, 4, 0]) mouse.update(buttons, undefined, send);
  assert.deepEqual(events.map(e => [e.button, e.down]), [[0,true],[1,true],[0,false],[2,true],[1,false],[2,false]]);
  assert.equal(mouse.held, false);
});
test("unsupported mouse buttons never become left clicks; reset forgets local holds", () => {
  const mouse = new RemoteMouseButtons(), events: RemoteInput[] = [];
  mouse.update(8, undefined, e => events.push(e)); assert.equal(events.length, 0);
  mouse.update(2, { x: .3, y: .4 }, e => events.push(e)); mouse.reset();
  mouse.update(0, undefined, e => events.push(e));
  assert.deepEqual(events, [{kind:"button",button:1,down:true,x:.3,y:.4}]);
});
test("relative motion sends immediately, coalesces trailing distance, and flushes before button release", t => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const events: RemoteInput[] = [], input = new RemoteMotionBuffer(e => events.push(e));
  input.input({ kind: "relativeMove", deltaX: 2, deltaY: 3 });
  assert.deepEqual(events, [{ kind: "relativeMove", deltaX: 2, deltaY: 3 }]);
  input.input({ kind: "relativeMove", deltaX: 4, deltaY: -1 });
  input.input({ kind: "relativeMove", deltaX: 7, deltaY: 8 });
  assert.equal(events.length, 1); t.mock.timers.tick(4);
  input.input({ kind: "relativeMove", deltaX: 9, deltaY: 10 });
  input.input({ kind: "button", button: 0, down: false });
  assert.deepEqual(events, [
    { kind: "relativeMove", deltaX: 2, deltaY: 3 },
    { kind: "relativeMove", deltaX: 11, deltaY: 7 },
    { kind: "relativeMove", deltaX: 9, deltaY: 10 },
    { kind: "button", button: 0, down: false },
  ]);
  t.mock.timers.tick(100); assert.equal(events.length, 4);
});
test("large trailing batches preserve displacement inside protocol bounds", t => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const events: RemoteInput[] = [], input = new RemoteMotionBuffer(e => events.push(e));
  input.input({ kind: "relativeMove", deltaX: 1, deltaY: -1 });
  input.input({ kind: "relativeMove", deltaX: 4000, deltaY: -3000 });
  input.input({ kind: "relativeMove", deltaX: 4000, deltaY: -3000 }); input.flush();
  assert.equal(events.length, 3);
  assert.equal(events.reduce((n, e) => n + (e.deltaX ?? 0), 0), 8001);
  assert.equal(events.reduce((n, e) => n + (e.deltaY ?? 0), 0), -6001);
  assert.ok(events.every(e => Math.abs(e.deltaX!) <= 4096 && Math.abs(e.deltaY!) <= 4096));
});
test("absolute motion keeps the leading and latest trailing sample; disconnect clears the rest", t => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const events: RemoteInput[] = [], input = new RemoteMotionBuffer(e => events.push(e));
  input.input({ kind: "move", x: .1, y: .2 });
  input.input({ kind: "move", x: .2, y: .3 }); input.input({ kind: "move", x: .3, y: .4 });
  t.mock.timers.tick(4);
  assert.deepEqual(events, [{ kind: "move", x: .1, y: .2 }, { kind: "move", x: .3, y: .4 }]);
  input.input({ kind: "relativeMove", deltaX: 10, deltaY: 10 }); input.clear();
  t.mock.timers.tick(100); assert.equal(events.length, 2);
});
test("sustained motion stays paced after trailing flushes and idle motion has no added wait", t => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const events: RemoteInput[] = [], input = new RemoteMotionBuffer(e => events.push(e));
  input.input({ kind: "relativeMove", deltaX: 1, deltaY: 0 });
  input.input({ kind: "relativeMove", deltaX: 2, deltaY: 0 });
  t.mock.timers.tick(4); assert.equal(events.length, 2);
  input.input({ kind: "relativeMove", deltaX: 3, deltaY: 0 });
  assert.equal(events.length, 2, "the trailing send must retain the next pacing window");
  t.mock.timers.tick(3); assert.equal(events.length, 2);
  t.mock.timers.tick(1); assert.equal(events.length, 3);
  t.mock.timers.tick(20);
  input.input({ kind: "relativeMove", deltaX: 4, deltaY: 0 });
  assert.equal(events.length, 4, "isolated motion sends synchronously after idle");
  assert.deepEqual(events.map(e => e.deltaX), [1, 2, 3, 4]); input.clear();
});
for (const event of [
  { kind: "key", key: 4, down: false }, { kind: "scroll", deltaX: 0, deltaY: 1 },
  { kind: "text", text: "fixture" },
] satisfies RemoteInput[]) {
  test(`${event.kind} flushes relative distance before its discrete boundary`, t => {
    t.mock.timers.enable({ apis: ["setTimeout"] });
    const events: RemoteInput[] = [], input = new RemoteMotionBuffer(e => events.push(e));
    input.input({ kind: "relativeMove", deltaX: 1, deltaY: 2 });
    input.input({ kind: "relativeMove", deltaX: 3, deltaY: 4 }); input.input(event);
    assert.deepEqual(events, [
      { kind: "relativeMove", deltaX: 1, deltaY: 2 },
      { kind: "relativeMove", deltaX: 3, deltaY: 4 }, event,
    ]);
    input.input({ kind: "relativeMove", deltaX: 5, deltaY: 6 });
    assert.equal(events.length, 4, "a new gesture can begin immediately after the boundary"); input.clear();
  });
}
test("releaseAll drops trailing motion and restores immediate leading dispatch", t => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const events: RemoteInput[] = [], input = new RemoteMotionBuffer(e => events.push(e));
  input.input({ kind: "move", x: .1, y: .2 }); input.input({ kind: "move", x: .8, y: .9 });
  input.input({ kind: "releaseAll" }); t.mock.timers.tick(20);
  assert.deepEqual(events, [{ kind: "move", x: .1, y: .2 }, { kind: "releaseAll" }]);
  input.input({ kind: "move", x: .4, y: .5 }); assert.equal(events.length, 3); input.clear();
});
test("motion mode transitions preserve relative distance and never merge with absolute coordinates", t => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const events: RemoteInput[] = [], input = new RemoteMotionBuffer(e => events.push(e));
  input.input({ kind: "relativeMove", deltaX: 0, deltaY: 0 });
  input.input({ kind: "relativeMove", deltaX: 5, deltaY: -5 });
  input.input({ kind: "move", x: .7, y: .8 });
  assert.deepEqual(events, [
    { kind: "relativeMove", deltaX: 0, deltaY: 0 },
    { kind: "relativeMove", deltaX: 5, deltaY: -5 }, { kind: "move", x: .7, y: .8 },
  ]); input.clear();
});
test("synchronous teardown during leading dispatch cancels its pacing window", t => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const events: RemoteInput[] = [];
  const input = new RemoteMotionBuffer(e => { events.push(e); input.clear(); });
  input.input({ kind: "move", x: .1, y: .2 });
  input.input({ kind: "move", x: .3, y: .4 });
  assert.equal(events.length, 2); t.mock.timers.tick(100); assert.equal(events.length, 2);
});
