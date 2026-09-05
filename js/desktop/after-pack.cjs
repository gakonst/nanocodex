const { execFile } = require("node:child_process");
const { readdir } = require("node:fs/promises");
const { join } = require("node:path");
const { promisify } = require("node:util");

const run = promisify(execFile);

// electron-builder renames helper executables and display names, but leaves
// CFBundleName as "Electron Helper". Keep every app bundle's identity consistent.
module.exports = async ({ electronPlatformName, appOutDir, packager }) => {
  if (electronPlatformName !== "darwin") return;
  const frameworks = join(
    appOutDir,
    `${packager.appInfo.productFilename}.app`,
    "Contents/Frameworks"
  );
  for (const name of await readdir(frameworks)) {
    if (!name.startsWith("Nanocodex Helper") || !name.endsWith(".app"))
      continue;
    const plist = join(frameworks, name, "Contents/Info.plist");
    const { stdout } = await run("/usr/libexec/PlistBuddy", [
      "-c",
      "Print :CFBundleDisplayName",
      plist,
    ]);
    await run("/usr/libexec/PlistBuddy", [
      "-c",
      `Set :CFBundleName ${stdout.trim()}`,
      plist,
    ]);
  }
};
