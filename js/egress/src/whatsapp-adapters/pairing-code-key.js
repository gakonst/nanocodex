import { Buffer } from 'node:buffer';
import { pbkdf2Async } from '@noble/hashes/pbkdf2.js';
import { sha256 } from '@noble/hashes/sha2.js';

// WhatsApp requires 131072 rounds. Workers caps native PBKDF2 at 100000;
// preserve the protocol exactly with the pinned, cooperative JS implementation.
export async function derivePairingCodeKey(pairingCode, salt) {
  const password = new TextEncoder().encode(pairingCode);
  const saltBytes = new Uint8Array(salt instanceof Uint8Array ? salt : new Uint8Array(salt));
  return Buffer.from(await pbkdf2Async(sha256, password, saltBytes, { c: 131072, dkLen: 32 }));
}
