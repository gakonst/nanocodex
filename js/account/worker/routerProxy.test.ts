import { test } from "node:test";
import assert from "node:assert/strict";
import { routeManaged } from "./managedProxy.ts";
test("inference keys cannot read deployment routing telemetry",async()=>{
 const url=new URL("https://example.com/api/router");
 const response=await routeManaged(new Request(url,{headers:{authorization:"Bearer nci_synthetic"}}),{},url);
 assert.equal(response?.status,403);
});
