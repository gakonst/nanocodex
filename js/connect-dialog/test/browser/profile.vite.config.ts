import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import { fileURLToPath } from 'node:url';
// Same public ConnectOnboarding fixture, compiled with production React/Vite.
export default defineConfig({
  root: fileURLToPath(new URL('.', import.meta.url)),
  base: '/fixture/',
  plugins: [react()],
  resolve: { dedupe: ['react', 'react-dom'] },
  build: { outDir: '../../../../output/connect-profile/fixture', emptyOutDir: true },
});
