const fs = require("fs"), path = require("path"), { spawn } = require("child_process");
// Stand-in for the server's container runtime (see the journey header).
const root = process.env.NCX_DOCKER_ROOT, args = process.argv.slice(2);
fs.appendFileSync(path.join(root, "docker-calls.jsonl"), JSON.stringify({ at: new Date().toISOString(), argv: args }) + "\n");
const file = name => path.join(root, name + ".json");
const load = name => { try { return JSON.parse(fs.readFileSync(file(name), "utf8")); } catch { return undefined; } };
const alive = pid => { try { process.kill(pid, 0); return true; } catch { return false; } };
const sleep = ms => Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
function stop(container) {
  if (!container || !container.pid || !alive(container.pid)) return;
  try { process.kill(-container.pid, "SIGTERM"); } catch {}
  for (let i = 0; i < 100 && alive(container.pid); i++) sleep(100);
  if (alive(container.pid)) try { process.kill(-container.pid, "SIGKILL"); } catch {}
}
function launch(container) {
  const map = value => {
    if (value === "/ssh-host-keys" || value.startsWith("/ssh-host-keys/")) return process.env.NCX_SERVER_SSH_DIR + value.slice(14);
    for (const mount of container.mounts) if (value === mount.dst || value.startsWith(mount.dst + "/")) return mount.src + value.slice(mount.dst.length);
    return value;
  };
  if (container.entrypoint !== "/usr/local/bin/nanocodex-remote") { process.stderr.write("unsupported entrypoint\n"); process.exit(125); }
  const home = path.join(root, container.name + "-home");
  fs.mkdirSync(home, { recursive: true, mode: 0o700 });
  const env = { PATH: process.env.NCX_CONTAINER_PATH, HOME: home };
  for (const entry of container.env) { const i = entry.indexOf("="); env[entry.slice(0, i)] = map(entry.slice(i + 1)); }
  const argv = container.args.map(value => value.startsWith("/") ? map(value) : value);
  const command = [...(process.env.NCX_WRAPPER || "").split(",").filter(Boolean), process.env.NCX_HAND_EXECUTABLE, ...argv];
  const log = fs.openSync(path.join(root, container.name + ".log"), "a");
  const child = spawn(command[0], command.slice(1), { env, cwd: map("/workspace"), detached: true, stdio: ["ignore", log, log] });
  container.pid = child.pid;
  container.starts = (container.starts || 0) + 1;
  fs.writeFileSync(file(container.name), JSON.stringify(container), { mode: 0o600 });
  child.unref();
}
const [command, ...rest] = args;
if (command === "info" || command === "pull") process.exit(0);
if (command === "image" && rest[0] === "inspect") process.exit(0);
if (command === "container" && rest[0] === "inspect") process.exit(load(rest[1]) ? 0 : 1);
if (command === "inspect" && rest[0] === "--format") {
  const container = load(rest[2]);
  if (!container) process.exit(1);
  const label = rest[1].match(/index \.Config\.Labels "([^"]+)"/);
  process.stdout.write(((label && container.labels[label[1]]) || "") + "\n");
  process.exit(0);
}
if (command === "rm" && rest[0] === "-f") { stop(load(rest[1])); fs.rmSync(file(rest[1]), { force: true }); process.exit(0); }
if (command === "restart") { const container = load(rest[0]); if (!container) process.exit(1); stop(container); launch(container); process.exit(0); }
if (command === "run") {
  const valued = new Set(["--name", "--label", "--restart", "--cap-drop", "--security-opt", "--pids-limit", "--memory", "--user", "--env", "--mount", "--entrypoint"]);
  const container = { labels: {}, env: [], mounts: [], options: [] };
  let i = 0;
  for (; i < rest.length && rest[i].startsWith("-"); i++) {
    const option = rest[i];
    if (option === "-d" || option === "--init") { container.options.push(option); continue; }
    if (!valued.has(option)) { process.stderr.write("unsupported docker run option " + option + "\n"); process.exit(125); }
    const value = rest[++i];
    if (option === "--name") container.name = value;
    else if (option === "--label") { const j = value.indexOf("="); container.labels[value.slice(0, j)] = value.slice(j + 1); }
    else if (option === "--env") container.env.push(value);
    else if (option === "--mount") {
      const spec = Object.fromEntries(value.split(",").map(part => { const j = part.indexOf("="); return j < 0 ? [part, true] : [part.slice(0, j), part.slice(j + 1)]; }));
      if (spec.type !== "bind" || !spec.src || !spec.dst) process.exit(125);
      container.mounts.push({ src: spec.src, dst: spec.dst, readonly: spec.readonly === true });
    } else if (option === "--entrypoint") container.entrypoint = value;
    else container.options.push(option + "=" + value);
  }
  container.image = rest[i];
  container.args = rest.slice(i + 1);
  if (!container.name || !container.image || load(container.name)) process.exit(125);
  launch(container);
  process.stdout.write("synthetic-container-id\n");
  process.exit(0);
}
process.stderr.write("unsupported docker command\n");
process.exit(125);
