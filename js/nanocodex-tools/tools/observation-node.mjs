import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { mkdtemp, readFile, writeFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { normalizeObservationFrame } from "./observation.mjs";

const execute = promisify(execFile);

/** Explicitly shares the local desktop or one named Android device with owned viewers. */
export function createScreenObservation({ source = "desktop", device, name = source === "android" ? "Android screen" : "Desktop" } = {}) {
  if (!["desktop", "android"].includes(source)) throw new TypeError("unsupported screen source");
  if (source === "android" && (typeof device !== "string" || !device || device.startsWith("-") || device.length > 256 || device.includes("\0"))) throw new TypeError("Android capture requires an explicit device serial");
  return Object.freeze({
    surfaces: Object.freeze([Object.freeze({ id: "screen", name, kind: source === "android" ? "phone" : "desktop" })]),
    async capture({ surfaceId, signal }) {
      if (surfaceId !== "screen") throw new Error("unknown screen");
      signal.throwIfAborted();
      const directory = await mkdtemp(join(tmpdir(), "nanocodex-screen-"));
      const raw = join(directory, "raw.png");
      const output = join(directory, "frame.jpg");
      const run = (command, args, maxBuffer = 64 * 1024) => execute(command, args, { signal, timeout: 4_000, maxBuffer, encoding: "buffer", windowsHide: true });
      try {
        let input;
        if (source === "android") {
          const { stdout } = await run("adb", ["-s", device, "exec-out", "screencap", "-p"], 32 * 1024 * 1024);
          await writeFile(raw, stdout, { mode: 0o600 });
          input = ["-i", raw];
        } else if (process.platform === "darwin") {
          await run("screencapture", ["-x", "-t", "png", raw]);
          input = ["-i", raw];
        } else if (process.platform === "win32") {
          input = ["-f", "gdigrab", "-i", "desktop"];
        } else if (process.env.WAYLAND_DISPLAY) {
          await run("grim", [raw]);
          input = ["-i", raw];
        } else if (process.env.DISPLAY) {
          input = ["-f", "x11grab", "-i", process.env.DISPLAY];
        } else throw new Error("No desktop display available");
        await run("ffmpeg", ["-nostdin", "-loglevel", "error", ...input, "-frames:v", "1", "-vf", "scale=960:960:force_original_aspect_ratio=decrease", "-q:v", "8", "-y", output]);
        const bytes = await readFile(output);
        const [width, height] = jpegSize(bytes);
        return normalizeObservationFrame({ captured_at: Date.now(), width, height, mime_type: "image/jpeg", data: bytes.toString("base64") });
      } finally { await rm(directory, { recursive: true, force: true }); }
    },
  });
}

function jpegSize(bytes) {
  if (bytes[0] !== 0xff || bytes[1] !== 0xd8) throw new Error("invalid JPEG");
  for (let offset = 2; offset + 9 < bytes.length;) {
    if (bytes[offset] !== 0xff) break;
    const marker = bytes[offset + 1];
    const length = bytes.readUInt16BE(offset + 2);
    if (length < 2) break;
    if ([0xc0, 0xc1, 0xc2].includes(marker)) return [bytes.readUInt16BE(offset + 7), bytes.readUInt16BE(offset + 5)];
    offset += length + 2;
  }
  throw new Error("missing JPEG dimensions");
}
