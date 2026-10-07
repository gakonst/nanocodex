import { restoreAuthBuffers } from './whatsapp-adapters/auth-buffers';
import makeWASocket, { Browsers, DisconnectReason, fetchLatestBaileysVersion, initAuthCreds, proto } from '@whiskeysockets/baileys';
import type { WhatsAppEvent, WhatsAppMessage, WhatsAppTransportFactory } from './whatsapp-transport';

// Auth is persisted exclusively through the account's encrypted external store.
// Never use upstream filesystem auth helpers, console logging, or expose send APIs.
const silent = { level: 'silent', trace() {}, debug() {}, info() {}, warn() {}, error() {}, fatal() {}, child() { return this; } };
export function createWhatsAppTransportFactory(): WhatsAppTransportFactory { return whatsappTransportFactory; }

export const whatsappTransportFactory: WhatsAppTransportFactory = {
  async connect(callbacks) {
    const saved = await callbacks.auth.credentials();
    const creds = saved ? restoreAuthBuffers(saved) : initAuthCreds();
    if (!saved) await callbacks.auth.saveCredentials(creds as unknown as Record<string, unknown>);
    let pending = Promise.resolve();
    let stopped = false;
    let persistenceFailed = false;
    let pairingReadyResolve!: () => void;
    let pairingReadyReject!: (error: Error) => void;
    const pairingReady = new Promise<void>((resolve, reject) => { pairingReadyResolve = resolve; pairingReadyReject = reject; });
    // A restored socket may close without ever requesting a pairing code.
    void pairingReady.catch(() => {});
    // WhatsApp rejects phone-number linking ("Couldn't link device") from stale
    // client versions and unrecognized companion platforms. Use the current
    // published web version when reachable with the Ubuntu desktop profile (macOS desktop makes WhatsApp close with 428 before issuing a code).
    const version = await currentWhatsAppVersion();
    const socket = makeWASocket({
      ...(version ? { version } : {}),
      auth: { creds: creds as ReturnType<typeof initAuthCreds>, keys: {
        async get(type, ids) {
          const values = restoreAuthBuffers(await callbacks.auth.getKeys(type, ids));
          if (type === 'app-state-sync-key') for (const id of Object.keys(values)) {
            if (values[id]) values[id] = proto.Message.AppStateSyncKeyData.fromObject(values[id] as Record<string, unknown>);
          }
          return values as any;
        },
        async set(data) { await callbacks.auth.setKeys(data as Record<string, Record<string, unknown | null>>); },
      } },
      logger: silent as any,
      markOnlineOnConnect: false,
      browser: Browsers.ubuntu('Desktop'),
      syncFullHistory: true,
      getMessage: async () => undefined,
    });
    const enqueue = (work: () => Promise<void>) => {
      pending = pending.then(work).catch(() => {
        stopped = true;
        persistenceFailed = true;
        socket.end(new Error('WhatsApp persistence failed'));
      });
    };
    const deliver = async (events: WhatsAppEvent[]) => {
      // Account writes are bounded; serialize each chunk before the next event.
      for (let offset = 0; offset < events.length; offset += 1000) {
        await callbacks.onEvents(events.slice(offset, offset + 1000));
      }
    };
    socket.ev.on('creds.update', update => enqueue(() => callbacks.auth.saveCredentials(update as Record<string, unknown>)));
    socket.ev.on('connection.update', update => {
      // WebSocket open precedes Noise initialization. A QR update proves the
      // encrypted pair-device exchange is ready; retain only this boolean signal.
      if (update.qr || update.connection === 'open') pairingReadyResolve();
      if (update.connection === 'close') pairingReadyReject(new Error('WhatsApp closed before pairing was ready'));
      if (!update.connection) return;
      const status = (update.lastDisconnect?.error as any)?.output?.statusCode;
      const terminal = status === DisconnectReason.loggedOut || status === DisconnectReason.badSession
        || status === DisconnectReason.connectionReplaced || status === DisconnectReason.multideviceMismatch
        || status === DisconnectReason.forbidden;
      enqueue(() => callbacks.onConnection({ state: update.connection!, loggedOut: status === DisconnectReason.loggedOut,
        retryable: !stopped && !terminal, restartRequired: status === DisconnectReason.restartRequired }));
    });
    socket.ev.on('messages.upsert', ({ messages }) => enqueue(() => deliver(messages.flatMap(projectMessage))));
    socket.ev.on('messages.update', updates => enqueue(async () => {
      const events: WhatsAppEvent[] = [];
      for (const { key, update } of updates) {
        if (!key.id || !key.remoteJid) continue;
        if (update.message === null) events.push({ type: 'revoke', chat_id: key.remoteJid, id: key.id, timestamp: Date.now() });
        else if (update.message) events.push(...projectMessage({ key, ...update }));
      }
      if (events.length) await deliver(events);
    }));
    socket.ev.on('messages.delete', deletion => enqueue(async () => {
      if (!('keys' in deletion)) return;
      const events: WhatsAppEvent[] = deletion.keys.flatMap(key => key.id && key.remoteJid
        ? [{ type: 'revoke' as const, chat_id: key.remoteJid, id: key.id, timestamp: Date.now() }] : []);
      if (events.length) await deliver(events);
    }));
    for (const event of ['contacts.upsert', 'contacts.update'] as const) socket.ev.on(event, contacts => enqueue(async () => {
      await deliver(contacts.flatMap(contact => contact.id ? [{ type: 'contact' as const,
        contact: { id: contact.id, ...((contact.name ?? contact.notify) ? { name: (contact.name ?? contact.notify)! } : {}) } }] : []));
    }));
    for (const event of ['chats.upsert', 'chats.update'] as const) socket.ev.on(event, chats => enqueue(async () => {
      await deliver(chats.flatMap(chat => chat.id ? [{ type: 'chat' as const,
        chat: { id: chat.id, ...(chat.name ? { name: chat.name } : {}) } }] : []));
    }));
    socket.ev.on('messaging-history.set', history => enqueue(async () => {
      const events: WhatsAppEvent[] = history.messages.flatMap(projectMessage);
      for (const chat of history.chats) if (chat.id) events.push({ type: 'chat', chat: { id: chat.id, ...(chat.name ? { name: chat.name } : {}) } });
      for (const contact of history.contacts) if (contact.id) events.push({ type: 'contact', contact: { id: contact.id, ...((contact.name ?? contact.notify) ? { name: (contact.name ?? contact.notify)! } : {}) } });
      const timestamps = events.flatMap(event => event.type === 'message' ? [event.message.timestamp] : []);
      events.push({ type: 'history', complete: false, ...(timestamps.length ? { oldest_timestamp: timestamps.reduce((oldest, timestamp) => Math.min(oldest, timestamp), Infinity) } : {}) });
      await deliver(events);
    }));
    return {
      async requestPairingCode(phone) {
        await socket.waitForSocketOpen();
        let timer: ReturnType<typeof setTimeout> | undefined;
        try {
          await Promise.race([pairingReady, new Promise<never>((_, reject) => {
            timer = setTimeout(() => reject(new Error('WhatsApp pairing readiness timed out')), 30000);
          })]);
        } finally { if (timer !== undefined) clearTimeout(timer); }
        const code = await socket.requestPairingCode(phone);
        await pending;
        if (persistenceFailed) throw new Error('WhatsApp persistence failed');
        return code;
      },
      async requestHistory(request) {
        await socket.fetchMessageHistory(request.limit, { remoteJid: request.chat_id, id: request.id, fromMe: request.from_me }, Math.floor(request.before / 1000));
      },
      async logout() { await socket.logout(); await pending; },
      async close() { stopped = true; socket.end(undefined); await pending; },
    };
  },
};

async function currentWhatsAppVersion(): Promise<[number, number, number] | undefined> {
  try {
    const latest = await Promise.race([
      fetchLatestBaileysVersion(),
      new Promise<undefined>(resolve => setTimeout(() => resolve(undefined), 5000)),
    ]);
    const version = latest?.version;
    return Array.isArray(version) && version.length === 3 && version.every(part => Number.isInteger(part) && part >= 0)
      ? version as [number, number, number] : undefined;
  } catch { return undefined; }
}

function projectMessage(value: proto.IWebMessageInfo): WhatsAppEvent[] {
  const key = value.key;
  if (!key?.id || !key.remoteJid || !value.message) return [];
  const timestamp = Number(value.messageTimestamp ?? 0) * 1000;
  let content = value.message;
  let viewOnce = false;
  let expires: number | undefined;
  for (let depth = 0; depth < 8; depth++) {
    if (content.ephemeralMessage?.message) { content = content.ephemeralMessage.message; continue; }
    const once = content.viewOnceMessage ?? content.viewOnceMessageV2 ?? content.viewOnceMessageV2Extension;
    if (once?.message) { viewOnce = true; content = once.message; continue; }
    break;
  }
  const protocol = content.protocolMessage;
  if (protocol?.editedMessage && protocol.key?.id) {
    return projectMessage({ ...value, key: { ...key, ...protocol.key }, message: protocol.editedMessage }).map(event =>
      event.type === 'message' ? { ...event, message: { ...event.message, revision: Number(protocol.timestampMs ?? Date.now()) } } : event);
  }
  if (protocol?.type === proto.Message.ProtocolMessage.Type.REVOKE && protocol.key?.id) {
    return [{ type: 'revoke', chat_id: protocol.key.remoteJid ?? key.remoteJid, id: protocol.key.id, timestamp }];
  }
  const context = content.extendedTextMessage?.contextInfo ?? content.imageMessage?.contextInfo ?? content.videoMessage?.contextInfo;
  if (context?.expiration) expires = timestamp + context.expiration * 1000;
  const text = content.conversation ?? content.extendedTextMessage?.text ?? content.imageMessage?.caption ?? content.videoMessage?.caption;
  const message: WhatsAppMessage = { id: key.id, chat_id: key.remoteJid, timestamp, sender: key.participant ?? key.remoteJid,
    from_me: key.fromMe ?? false, kind: Object.keys(content)[0] ?? 'unknown', view_once: viewOnce,
    ...(expires ? { expires_at: expires } : {}),
    ...(!viewOnce && text != null ? { text } : {}),
  };
  return [{ type: 'message', message }];
}
