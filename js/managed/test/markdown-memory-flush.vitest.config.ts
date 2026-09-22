import { cloudflareTest } from '@cloudflare/vitest-pool-workers';
import { defineConfig } from 'vitest/config';

export default defineConfig({
  plugins: [cloudflareTest({
    main: './test/markdown-memory-flush-worker.ts',
    miniflare: {
      compatibilityDate: '2026-07-29', compatibilityFlags: ['nodejs_compat'],
      durableObjects: { NANOCODEX_SESSIONS: { className: 'MemoryFlushFixture', useSQLite: true } },
    },
  })],
  test: { include: ['test/markdown-memory-flush.test.ts', 'test/markdown-memory-ai.test.ts'] },
});
