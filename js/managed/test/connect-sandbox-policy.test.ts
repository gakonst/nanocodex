import { describe, expect, it, vi } from "vitest";
import { createManagedNamespaceTools } from "../src/index";
import { managedMountTool } from "../src/mount-tool";
import {
  turnCanUseExecutionNamespace, turnCanProvisionExecutionProvider,
  executionMountAllowed, executionMountPeers,
} from "../src/execution-policy";

// Isolation failure modes: a missing scope enables native execution; a sandbox
// scope allocates a VM; name/replay/root reuse adopts an account/other-grant mount;
// an allowed sandbox's FUSE peers expose those mounts; captured tools outlive scope.
const grantId = `0x${"a".repeat(64)}`;
const account = { capabilities: ["agents:write", "tools:use"] };
const connect = { ...account, connectGrant: { grantId, sandboxExecution: true } };
const mount = (id: string, owner?: string, provider = "cloudflare") => ({
  id, provider, root: `/${id}`, workspace: "/workspace",
  configuration_json: JSON.stringify(owner === undefined ? {} : { connect_grant_id: owner }),
});
const own = mount("own", grantId), peer = mount("peer", grantId);
const foreign = mount("foreign", `0x${"b".repeat(64)}`), personal = mount("personal"), vm = mount("vm", grantId, "host");
const mounts = [own, peer, foreign, personal, vm];
const context = (id: string) => ({ sessionId: "session", parentCallId: id, callId: id, model: "fixture", signal: new AbortController().signal });

describe("Connect sandbox authority", () => {
  it("rejects missing scope and VM allocation at the mount tool boundary", async () => {
    let authorization: typeof connect | typeof account | undefined = connect;
    const provision = vi.fn(async (request) => ({ id: "own", name: request.name, provider: request.provider, mount: "/own", status: "mounted" as const, created: true }));
    const tool = managedMountTool(async (request) => {
      if (!turnCanProvisionExecutionProvider(authorization, request.provider)) throw Error("mount_forbidden");
      return provision(request);
    });
    await expect(tool.handler({ provider: "cf_sandbox", name: "cad" }, context("allowed"))).resolves.toMatchObject({ provider: "cf_sandbox" });
    for (const provider of ["linux-fixture", "mac-fixture"]) {
      await expect(tool.handler({ provider, name: "cad" }, context(provider))).rejects.toThrow("mount_forbidden");
    }
    for (const denied of [undefined, { ...connect, connectGrant: { grantId } }, { ...connect, capabilities: ["agents:write"] }]) {
      authorization = denied as typeof connect;
      await expect(tool.handler({ provider: "cf_sandbox", name: "cad" }, context("denied"))).rejects.toThrow("mount_forbidden");
    }
    expect(provision).toHaveBeenCalledTimes(1);
    authorization = account;
    await expect(tool.handler({ provider: "linux-fixture", name: "vm" }, context("account"))).resolves.toMatchObject({ provider: "linux-fixture" });
  });

  it("isolates retained roots, direct dispatch, and FUSE peers by immutable grant ownership", async () => {
    expect(mounts.filter(m => executionMountAllowed(connect, m))).toEqual([own, peer]);
    expect(mounts.filter(m => executionMountAllowed(account, m))).toEqual([personal]);
    expect(executionMountPeers(own, mounts)).toEqual([own, peer]);
    expect(executionMountPeers(personal, mounts)).toEqual([personal]);
    expect(executionMountAllowed(connect, { ...own, configuration_json: '{"connect_grant_id":true}' })).toBe(false);
    let authorization = connect;
    const invoke = vi.fn(async () => ({ output: "native-sandbox" }));
    const tools = createManagedNamespaceTools(
      () => turnCanUseExecutionNamespace(authorization),
      () => mounts.filter(m => executionMountAllowed(authorization, m)),
      (id) => mounts.some(m => m.id === id && executionMountAllowed(authorization, m)) ? { handler: invoke } : undefined,
    );
    const exec = tools.find(t => t.name === "exec_command")!;
    await expect(exec.handler({ cmd: "pwd", workdir: own.root }, context("own"))).resolves.toEqual({ output: "native-sandbox" });
    for (const target of [personal, foreign, vm]) {
      await expect(exec.handler({ cmd: "pwd", workdir: target.root }, context(target.id))).rejects.toThrow();
    }
    authorization = { ...connect, connectGrant: { grantId, sandboxExecution: false } };
    await expect(exec.handler({ cmd: "pwd", workdir: own.root }, context("own"))).rejects.toThrow();
    expect(invoke).toHaveBeenCalledTimes(1);
  });
  // A retained native process and a repeated cell id must not carry authority
  // into a later turn admitted under another grant or full account authority.
  it("keeps process sessions and captured cells within their originating authorization", async () => {
    let authorization: typeof connect | typeof account = connect;
    const write = vi.fn(async () => ({ output: "private output", session_id: 7, wall_time_seconds: 0 }));
    const exec = vi.fn(async () => ({ output: "started", session_id: 7, wall_time_seconds: 0 }));
    const tools = createManagedNamespaceTools(
      () => turnCanUseExecutionNamespace(authorization),
      () => mounts.filter(m => executionMountAllowed(authorization, m)),
      (id, name) => mounts.some(m => m.id === id && executionMountAllowed(authorization, m))
        ? { handler: name === "write_stdin" ? write : exec } : undefined,
      undefined, undefined, undefined,
      () => "connectGrant" in authorization ? authorization.connectGrant.grantId : "account",
    );
    const execute = tools.find(t => t.name === "exec_command")!;
    const stdin = tools.find(t => t.name === "write_stdin")!;
    const started = await execute.handler({ cmd: "long", workdir: own.root }, context("reused-cell")) as { session_id: number };
    await expect(stdin.handler({ session_id: started.session_id }, context("same-grant"))).resolves.toMatchObject({ output: "private output" });
    for (const denied of [account, { ...connect, connectGrant: { grantId: `0x${"b".repeat(64)}`, sandboxExecution: true } }]) {
      authorization = denied;
      await expect(stdin.handler({ session_id: started.session_id, chars: "\u0003" }, context("poll"))).rejects.toThrow();
      await expect(execute.handler({ cmd: "pwd", workdir: own.root }, context("reused-cell"))).rejects.toThrow();
    }
    expect(write).toHaveBeenCalledTimes(1);
    expect(exec).toHaveBeenCalledTimes(1);
    authorization = connect;
    await expect(stdin.handler({ session_id: started.session_id }, context("resume"))).resolves.toMatchObject({ session_id: started.session_id });
  });

});
