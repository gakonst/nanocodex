import { defineConfig, mergeConfig } from 'vitest/config';
import managed from './vitest.config.ts';

// Keep the production Worker bindings/module graph while selecting memory checks.
const configuration = mergeConfig(managed, defineConfig({ test: { fileParallelism: false } }));
configuration.test = { ...configuration.test,
  include: ['test/markdown-memory*.test.ts', 'test/personal-memory.test.ts',
    'test/personalization.test.ts', 'test/voice-personalization.test.ts', 'test/startup-context.test.ts',
    'test/managed-turn-input.test.ts', 'test/memory-scope-isolation.test.ts',
    'test/memory-session-tools.test.ts', 'test/default-mcp.test.ts'],
};
export default configuration;
