import { readdir, lstat, readFile, mkdir, open } from 'node:fs/promises';
import { resolve, join, relative, isAbsolute, sep } from 'node:path';
import { fileMemoriesBackend } from './extensions.mjs';
/** An explicitly selected private memory directory. All writes use exclusive create. */
export function nodeMemoriesBackend(root) {
  root = resolve(root);
  async function checked(path, create = false) {
    const target = resolve(root, path);
    const rel = relative(root, target);
    if (isAbsolute(rel) || rel.split(sep).includes('..')) throw new Error('path escapes memory root');
    // Include root itself: a symlink root must not grant access to another store.
    let current = root;
    for (const component of ['', ...rel.split(sep).filter(Boolean)]) {
      if (component) current = join(current, component);
      const stat = await lstat(current).catch(error => { if (error.code === 'ENOENT') return null; throw error; });
      if (stat?.isSymbolicLink()) throw new Error('memory path must not be a symlink');
      if (!stat && create) await mkdir(current);
    }
    return target;
  }
  async function tree() {
    const files = [], directories = [];
    const pending = [await checked('')];
    while (pending.length) {
      const directory = pending.pop();
      for (const entry of await readdir(directory, { withFileTypes: true })) {
        if (entry.name.startsWith('.') || entry.isSymbolicLink()) continue;
        const path = join(directory, entry.name);
        const name = relative(root, path).split(sep).join('/');
        if (entry.isDirectory()) { pending.push(path); directories.push(name); }
        else if (entry.isFile()) files.push(name);
      }
    }
    return { files, directories };
  }
  return fileMemoriesBackend({
    listFiles: async () => (await tree()).files,
    listDirectories: async () => (await tree()).directories,
    readFile: async path => readFile(await checked(path), 'utf8'),
    createFile: async (path, content) => {
      await checked(path.split('/').slice(0,-1).join('/'), true);
      const target = await checked(path);
      const file = await open(target, 'wx', 0o600);
      try { await file.writeFile(content); } finally { await file.close(); }
    },
  });
}
