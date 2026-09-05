import { mkdir, readFile, rename, writeFile } from "node:fs/promises";
import { homedir } from "node:os";
import { dirname, join } from "node:path";

import { createJsonChannelStore } from "mppx/client";

export function delegatedAccessKey(rootAddress, account) {
  const accessKeyAddress = account?.accessKeyAddress;
  if (typeof accessKeyAddress !== "string") {
    throw new Error("Tempo Accounts SDK did not select a delegated access key");
  }
  if (accessKeyAddress.toLowerCase() === rootAddress.toLowerCase()) {
    throw new Error("Tempo Accounts SDK selected the root wallet instead of an access key");
  }
  return accessKeyAddress;
}

export function persistentChannelStore(
  file = join(homedir(), ".tempo", "wallet", "nanocodex-mpp-channels.json"),
) {
  const kv = {
    async get(key) {
      return (await readChannels(file))[key];
    },
    async set(key, value) {
      const channels = await readChannels(file);
      channels[key] = value;
      await writeChannels(file, channels);
    },
    async delete(key) {
      const channels = await readChannels(file);
      delete channels[key];
      await writeChannels(file, channels);
    },
  };
  return createJsonChannelStore(kv);
}

async function readChannels(file) {
  try {
    const value = JSON.parse(await readFile(file, "utf8"));
    return value && typeof value === "object" && !Array.isArray(value) ? value : {};
  } catch (error) {
    if (error?.code === "ENOENT") return {};
    throw new Error(`failed to read MPP channel store ${file}`, { cause: error });
  }
}

async function writeChannels(file, channels) {
  await mkdir(dirname(file), { recursive: true, mode: 0o700 });
  const temporary = `${file}.${process.pid}.tmp`;
  await writeFile(temporary, `${JSON.stringify(channels)}\n`, { mode: 0o600 });
  await rename(temporary, file);
}
