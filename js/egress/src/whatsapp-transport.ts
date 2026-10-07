/** Workers-only upstream boundary. Implementations must never log auth or pairing codes. */
export interface WhatsAppAuthStore {
  credentials(): Promise<Record<string, unknown> | null>;
  /** Merge credentials and commit before resolving. Uint8Arrays are preserved. */
  saveCredentials(update: Record<string, unknown>): Promise<void>;
  getKeys(type: string, ids: string[]): Promise<Record<string, unknown>>;
  /** A batch is committed atomically; null removes a key. */
  setKeys(data: Record<string, Record<string, unknown | null>>): Promise<void>;
}
export type WhatsAppMessage = {
  id: string; chat_id: string; timestamp: number; sender?: string; from_me?: boolean;
  text?: string; kind?: string; view_once?: boolean; expires_at?: number;
  revision?: number; revoked?: boolean;
};
export type WhatsAppEvent =
  | { type: "message"; message: WhatsAppMessage }
  | { type: "revoke"; chat_id: string; id: string; timestamp: number }
  | { type: "chat"; chat: { id: string; name?: string; timestamp?: number } }
  | { type: "contact"; contact: { id: string; name?: string } }
  | { type: "history"; complete: boolean; oldest_timestamp?: number };
export interface WhatsAppTransportCallbacks {
  auth: WhatsAppAuthStore;
  onConnection(update: { state: "connecting" | "open" | "close"; loggedOut?: boolean; retryable?: boolean; restartRequired?: boolean }): Promise<void>;
  onEvents(events: WhatsAppEvent[]): Promise<void>;
}
export interface WhatsAppTransport {
  requestPairingCode(phone: string): Promise<string>;
  /** Unlink this device upstream. A failure is an unknown remote outcome. */
  logout(): Promise<void>;
  close(): Promise<void>;
  requestHistory?(request: { chat_id: string; before: number; limit: number; id: string; from_me: boolean }): Promise<void>;
}
export interface WhatsAppTransportFactory {
  connect(callbacks: WhatsAppTransportCallbacks): Promise<WhatsAppTransport>;
}
export type WhatsAppStatus = {
  id: string; connection_id: string; label: string; connected: boolean; socket_connected: boolean;
  state: "disconnected" | "connecting" | "pairing" | "connected" | "reconnecting" | "revoked";
  attempt: { operation_id: string; state: "requested" | "ready" | "expired" | "unknown" | "paired"; expires_at: number } | null;
  coverage: { source: "linked_device"; complete: boolean; history_complete: boolean; oldest_timestamp: number | null; last_received_at: number | null; note: string };
  retry_at: number | null;
};
