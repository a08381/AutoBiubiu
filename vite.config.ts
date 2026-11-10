import { svelte } from '@sveltejs/vite-plugin-svelte';
import { defineConfig } from 'vitest/config';

const host = process.env.TAURI_DEV_HOST;
const debug = process.env.TAURI_ENV_DEBUG === 'true' || process.env.TAURI_ENV_DEBUG === '1';

export default defineConfig({
  plugins: [svelte()],
  // 保留 Rust 编译输出；仅构建明确导入的 Svelte 与静态资源。
  clearScreen: false,
  publicDir: false,
  server: {
    host: host || '127.0.0.1',
    port: 1420,
    strictPort: true,
    hmr: host ? { protocol: 'ws', host, port: 1421 } : undefined,
    watch: { ignored: ['**/src-tauri/**', '**/data/**', '**/dist/**', '**/build/**'] },
  },
  envPrefix: ['VITE_', 'TAURI_ENV_'],
  build: {
    outDir: 'build',
    target: 'chrome105',
    minify: debug ? false : 'esbuild',
    sourcemap: debug,
  },
  test: {
    include: ['test/frontend.test.ts'],
    environment: 'node',
    clearMocks: true,
  },
});
