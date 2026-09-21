import { test } from "node:test";
import assert from "node:assert/strict";
import { routeManaged, isManagedRoutePath } from "./managedProxy.ts";
import { documentStatusForPath } from "./linkPreview.ts";
test("router page and API bypass the HTML fallback",()=>{assert.equal(documentStatusForPath("/router"),200);assert.equal(isManagedRoutePath("/api/router"),true);assert.equal(isManagedRoutePath("/v1/router"),true);});
test("inference keys cannot read deployment routing telemetry",async()=>{
 const url=new URL("https://example.com/api/router");
 const response=await routeManaged(new Request(url,{headers:{authorization:"Bearer nci_synthetic"}}),{},url);
 assert.equal(response?.status,403);
});
test("dashboard proxy preserves authentication and query validation",async()=>{
 const url=new URL("https://example.com/api/router?invalid=1");
 const result=await routeManaged(new Request(url,{headers:{cookie:"synthetic=session"}}),{NANOCODEX_BACKEND:{fetch:async(request:Request)=>{
 assert.equal(new URL(request.url).pathname,"/v1/router");assert.equal(new URL(request.url).search,"?invalid=1");assert.equal(request.headers.get("cookie"),"synthetic=session");return Response.json({ok:true});}} as unknown as Fetcher},url);
 assert.equal(result?.status,200);
});
