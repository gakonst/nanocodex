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
test("relative motion batches at 4ms and flushes before independent releases", t => {
  t.mock.timers.enable({apis:["setTimeout"]});
  const events: RemoteInput[] = [], input = new RemoteMotionBuffer(e => events.push(e));
  input.input({kind:"relativeMove",deltaX:2,deltaY:3}); input.input({kind:"relativeMove",deltaX:4,deltaY:-1});
  assert.equal(events.length,0); t.mock.timers.tick(4);
  input.input({kind:"relativeMove",deltaX:7,deltaY:8}); input.input({kind:"button",button:0,down:false});
  assert.deepEqual(events,[{kind:"relativeMove",deltaX:6,deltaY:2},{kind:"relativeMove",deltaX:7,deltaY:8},{kind:"button",button:0,down:false}]);
});
test("large batches preserve displacement inside protocol bounds; release discards pending motion", t => {
  t.mock.timers.enable({apis:["setTimeout"]});
  const events: RemoteInput[] = [], input = new RemoteMotionBuffer(e => events.push(e));
  input.input({kind:"relativeMove",deltaX:4000,deltaY:-3000}); input.input({kind:"relativeMove",deltaX:4000,deltaY:-3000}); input.flush();
  assert.equal(events.reduce((n,e)=>n+(e.deltaX??0),0),8000); assert.equal(events.reduce((n,e)=>n+(e.deltaY??0),0),-6000);
  assert.ok(events.every(e=>Math.abs(e.deltaX!)<=4096 && Math.abs(e.deltaY!)<=4096));
  input.input({kind:"move",x:.1,y:.1}); input.input({kind:"releaseAll"}); t.mock.timers.tick(10);
  assert.equal(events.at(-1)?.kind,"releaseAll"); assert.equal(events.length,3);
});
test("absolute motion keeps the latest sample and clears on disconnect", t => {
  t.mock.timers.enable({apis:["setTimeout"]});
  const events: RemoteInput[] = [], input = new RemoteMotionBuffer(e=>events.push(e));
  input.input({kind:"move",x:.1,y:.2});input.input({kind:"move",x:.3,y:.4}); t.mock.timers.tick(4);
  assert.deepEqual(events,[{kind:"move",x:.3,y:.4}]);
  input.input({kind:"relativeMove",deltaX:10,deltaY:10});input.clear();t.mock.timers.tick(10);assert.equal(events.length,1);
});
