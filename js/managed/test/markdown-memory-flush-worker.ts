import { DurableObject } from 'cloudflare:workers';

/** Real SQLite storage for memory extraction tests, independent of the agent WASM build. */
export class MemoryFlushFixture extends DurableObject {
  async fetch() { return new Response('fixture'); }
}
