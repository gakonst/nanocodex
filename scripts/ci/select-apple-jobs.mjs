import { appendFileSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { changedPaths } from './select-jobs.mjs';

export function selectAppleJobs(paths) {
  const selected = { macos: false, ios: false };
  for (const path of paths) {
    if (/^(?:docs\/|.*\.(?:md|png|jpg)$)/.test(path) && !path.startsWith('apple/') && !path.startsWith('macos/')) continue;
    if (/^(?:js\/|hands\/remote\/|macos\/|bin\/nanocodex\/|crates\/experimental\/nanocodex-computer\/|pnpm-|package\.json|turbo\.json)/.test(path)) {
      selected.macos = true;
    } else if (/^(?:apple\/|Cargo\.|rust-toolchain|crates\/nanocodex-voice-|scripts\/ci\/|\.github\/workflows\/apple-inbox\.yml)/.test(path)) {
      selected.macos = selected.ios = true;
    } else {
      // Trigger additions and unfamiliar shared inputs must retain both builds.
      selected.macos = selected.ios = true;
    }
  }
  return selected;
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  let selected;
  try { selected = selectAppleJobs(changedPaths(process.env.GITHUB_EVENT_NAME, JSON.parse(readFileSync(process.env.GITHUB_EVENT_PATH, 'utf8')))); }
  catch { selected = { macos: true, ios: true }; }
  const output = Object.entries(selected).map(([key, value]) => `${key}=${value}`).join('\n') + '\n';
  if (process.env.GITHUB_OUTPUT) appendFileSync(process.env.GITHUB_OUTPUT, output);
  console.log(output);
}
