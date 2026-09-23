import { MarkdownMemoryStore } from './markdown-memory';
import { fileMemoriesBackend, type MemoryFileStore } from 'nanocodex-tools/extensions';
import { initializeTurnInputs, readTurnInput, storeTurnInput } from './managed-turn-input';

/** One already-authorized MemoryScope partition. Existing records remain in place. */
export function scopeMemoryFiles(storage: DurableObjectStorage, owner: string): MemoryFileStore {
  storage.sql.exec(`CREATE TABLE IF NOT EXISTS extension_memory_files (
    owner TEXT NOT NULL, path TEXT NOT NULL, content_json TEXT NOT NULL, PRIMARY KEY(owner,path)
  )`);
  const chunks = 'extension_memory_file_chunks';
  initializeTurnInputs(storage, chunks);
  const id = (path: string) => JSON.stringify([owner, path]);
  const markdown = new MarkdownMemoryStore(storage);
  return {
    listFiles: async () => [
      ...markdown.list(owner),
      ...storage.sql.exec<{ path: string }>('SELECT path FROM extension_memory_files WHERE owner=? ORDER BY path', owner).toArray().map(row => row.path),
    ],
    readFile: async path => {
      if (path === 'MEMORY.md' || path === 'USER.md' || path === 'DREAMS.md' || path.startsWith('memory/')) return markdown.readFile(owner, path);
      const row = storage.sql.exec<{ content_json: string }>(
        'SELECT content_json FROM extension_memory_files WHERE owner=? AND path=?', owner, path,
      ).toArray()[0];
      if (!row) throw new Error('memory file was not found');
      return JSON.parse(readTurnInput(storage, id(path), row.content_json, chunks)) as string;
    },
    createFile: async (path, content) => storage.transactionSync(() => {
      if (!path.startsWith('extensions/ad_hoc/notes/')) throw new Error('only ad-hoc notes may be created');
      if (storage.sql.exec('SELECT 1 FROM extension_memory_files WHERE owner=? AND path=?', owner, path).toArray().length) throw new Error('ad-hoc note already exists');
      const body = storeTurnInput(storage, id(path), JSON.stringify(content), chunks);
      storage.sql.exec('INSERT INTO extension_memory_files(owner,path,content_json) VALUES(?,?,?)', owner, path, body);
    }),
  };
}
export function scopeFileMemories(storage: DurableObjectStorage, owner: string): ReturnType<typeof fileMemoriesBackend> {
  const files = scopeMemoryFiles(storage, owner);
  const backend = fileMemoriesBackend(files);
  // The audit remains explicitly readable/listable, but never becomes recall evidence.
  const recall = fileMemoriesBackend({ ...files,
    listFiles: async () => (await files.listFiles()).filter(path => path !== 'DREAMS.md'),
  });
  return { ...backend, search: recall.search! };
}
