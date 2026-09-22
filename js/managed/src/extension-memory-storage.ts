import { fileMemoriesBackend, type MemoryFileStore } from 'nanocodex-tools/extensions';
import { initializeTurnInputs, readTurnInput, storeTurnInput } from './managed-turn-input';

type LegacyMemory = { key: { id: number; version: number }; content: string };
export type LegacyMemories = { list(): Promise<LegacyMemory[]>; read(id: number, version: number): string | undefined };
/** One already-authorized MemoryScope partition. Existing records remain in place. */
export function scopeMemoryFiles(storage: DurableObjectStorage, owner: string, legacy: LegacyMemories): MemoryFileStore {
  storage.sql.exec(`CREATE TABLE IF NOT EXISTS extension_memory_files (
    owner TEXT NOT NULL, path TEXT NOT NULL, content_json TEXT NOT NULL, PRIMARY KEY(owner,path)
  )`);
  const chunks = 'extension_memory_file_chunks';
  initializeTurnInputs(storage, chunks);
  const id = (path: string) => JSON.stringify([owner, path]);
  return {
    listFiles: async () => [
      ...storage.sql.exec<{ path: string }>('SELECT path FROM extension_memory_files WHERE owner=? ORDER BY path', owner).toArray().map(row => row.path),
      ...(await legacy.list()).map(memory => `legacy/${memory.key.id}-v${memory.key.version}.md`),
    ],
    readFile: async path => {
      const match = /^legacy\/(\d+)-v(\d+)\.md$/.exec(path);
      if (match) {
        const content = legacy.read(Number(match[1]), Number(match[2]));
        if (content === undefined) throw new Error('memory file was not found');
        return content;
      }
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
export const scopeFileMemories = (storage: DurableObjectStorage, owner: string, legacy: LegacyMemories) => fileMemoriesBackend(scopeMemoryFiles(storage, owner, legacy));
