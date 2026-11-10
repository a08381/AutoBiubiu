import { fileURLToPath } from 'node:url';
import { run } from './toolchain.mjs';

run(process.execPath, [fileURLToPath(new URL('../node_modules/@tauri-apps/cli/tauri.js', import.meta.url)), ...process.argv.slice(2)]);
