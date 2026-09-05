export type LocalConversation = Readonly<{
  id: string;
  title: string;
  createdAt: number;
  updatedAt: number;
  turnCount: number;
}>;

const CATALOG_KEY = "nanocodex.local-conversations.v1";
const BROWSER_THREAD_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;

export function loadLocalConversations(currentId: string): readonly LocalConversation[] {
  const retained = decodeCatalog(safeGet(CATALOG_KEY));
  if (retained.some(({ id }) => id === currentId)) return sorted(retained);
  const now = Date.now();
  const next = sorted([...retained, {
    id: currentId,
    title: "New conversation",
    createdAt: now,
    updatedAt: now,
    turnCount: 0,
  }]);
  persist(next);
  return next;
}

export function createLocalConversation(current: readonly LocalConversation[]) {
  const now = Date.now();
  const conversation: LocalConversation = Object.freeze({
    id: crypto.randomUUID(),
    title: "New conversation",
    createdAt: now,
    updatedAt: now,
    turnCount: 0,
  });
  const conversations = sorted([conversation, ...current]);
  persist(conversations);
  return { conversation, conversations } as const;
}

export function recordLocalConversationPrompt(
  current: readonly LocalConversation[],
  id: string,
  prompt: string,
): readonly LocalConversation[] {
  const conversations = sorted(current.map((item) => item.id === id ? {
    ...item,
    title: item.turnCount === 0 ? conversationTitle(prompt) : item.title,
    updatedAt: Date.now(),
    turnCount: item.turnCount + 1,
  } : item));
  persist(conversations);
  return conversations;
}

export function conversationTitle(input: string): string {
  const text = input.replace(/\s+/g, " ").trim();
  if (!text) return "New conversation";
  return text.length > 56 ? `${text.slice(0, 55).trimEnd()}…` : text;
}

function decodeCatalog(raw: string | null): readonly LocalConversation[] {
  if (!raw) return [];
  try {
    const parsed: unknown = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return [];
    const data = (parsed as { version?: unknown; conversations?: unknown }).conversations;
    if ((parsed as { version?: unknown }).version !== 1 || !Array.isArray(data)) return [];
    return data.flatMap((item) => {
      if (!item || typeof item !== "object" || Array.isArray(item)) return [];
      const value = item as Partial<LocalConversation>;
      return typeof value.id === "string" && BROWSER_THREAD_ID.test(value.id)
        && typeof value.title === "string"
        && typeof value.createdAt === "number" && Number.isFinite(value.createdAt)
        && typeof value.updatedAt === "number" && Number.isFinite(value.updatedAt)
        && Number.isSafeInteger(value.turnCount) && value.turnCount! >= 0
        ? [Object.freeze(value as LocalConversation)]
        : [];
    });
  } catch { return []; }
}

function sorted(items: readonly LocalConversation[]): readonly LocalConversation[] {
  return Object.freeze([...items].sort((a, b) => b.updatedAt - a.updatedAt || b.createdAt - a.createdAt));
}

function persist(conversations: readonly LocalConversation[]): void {
  try { localStorage.setItem(CATALOG_KEY, JSON.stringify({ version: 1, conversations })); } catch {}
}

function safeGet(key: string): string | null {
  try { return localStorage.getItem(key); } catch { return null; }
}
