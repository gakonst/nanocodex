import { defineConfig, mergeConfig } from 'vitest/config';
import managed from './vitest.config.ts';

// Keep the production Worker bindings/module graph while selecting memory checks.
const configuration = mergeConfig(managed, defineConfig({ test: { fileParallelism: false } }));
configuration.test = { ...configuration.test,
  include: ['test/markdown-memory*.test.ts'],
};
export default configuration;
