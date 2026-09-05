import { execFile, execFileSync } from "node:child_process";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);
const appPid = Number(process.argv[2]);
if (!Number.isInteger(appPid) || appPid < 2) process.exit(2);

const observed = new Map();
while (alive(appPid)) {
  await discoverEngineDescendants(appPid, observed);
  for (const [pid, command] of observed) {
    if (await commandFor(pid) !== command) observed.delete(pid);
  }
  await delay(250);
}

if (observed.size > 0) {
  for (const [pid, command] of observed) terminateIfUnchanged(pid, command, "SIGTERM");
  await delay(1_000);
  for (const [pid, command] of observed) terminateIfUnchanged(pid, command, "SIGKILL");
}

async function discoverEngineDescendants(parentPid, destination) {
  const pending = [parentPid];
  while (pending.length > 0) {
    const parent = pending.pop();
    if (parent === undefined) break;
    const children = await childPids(parent);
    for (const pid of children) {
      const command = await commandFor(pid);
      if (command && /(?:^|\/)rivet-engine(?:\.exe)? start(?:\s|$)/.test(command)) {
        destination.set(pid, command);
      } else {
        pending.push(pid);
      }
    }
  }
}

async function childPids(parentPid) {
  let stdout;
  try {
    ({ stdout } = await execFileAsync("pgrep", ["-P", String(parentPid)]));
  } catch (error) {
    if (error?.code === 1) return [];
    throw error;
  }
  return stdout.trim().split(/\s+/).map(Number).filter(Number.isInteger);
}

function terminateIfUnchanged(pid, expectedCommand, signal) {
  if (commandForSync(pid) !== expectedCommand) return;
  try {
    process.kill(pid, signal);
  } catch (error) {
    if (error?.code !== "ESRCH") throw error;
  }
}

async function commandFor(pid) {
  try {
    const { stdout } = await execFileAsync("ps", ["-p", String(pid), "-o", "command="]);
    return stdout.trim() || undefined;
  } catch {
    return undefined;
  }
}

function commandForSync(pid) {
  try {
    return execFileSync("ps", ["-p", String(pid), "-o", "command="], { encoding: "utf8" })
      .trim() || undefined;
  } catch {
    return undefined;
  }
}

function alive(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return error?.code === "EPERM";
  }
}

function delay(milliseconds) {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}
