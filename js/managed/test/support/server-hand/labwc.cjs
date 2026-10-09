const net = require("net"), path = require("path");
// Stand-in for the headless compositor: provides its Wayland socket and exits with its parent.
const parent = process.ppid;
net.createServer(socket => socket.destroy()).listen(path.join(process.env.XDG_RUNTIME_DIR, "wayland-0"));
setInterval(() => { try { process.kill(parent, 0); } catch { process.exit(0); } }, 500);
