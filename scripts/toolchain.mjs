import { homedir } from 'node:os';
import path from 'node:path';
import { spawn } from 'node:child_process';

// 新装 Rust 后无需重开编辑器；仍尊重用户指定的 CARGO_HOME。
export function run(command, args) {
  const env = { ...process.env };
  const key = Object.keys(env).find((name) => name.toLowerCase() === 'path') || 'PATH';
  env[key] = `${path.join(env.CARGO_HOME || path.join(homedir(), '.cargo'), 'bin')}${path.delimiter}${env[key] || ''}`;
  const child = spawn(command, args, { env, stdio: 'inherit' });
  child.on('error', (error) => { console.error(`启动开发工具失败：${error.message}`); process.exitCode = 1; });
  child.on('exit', (code) => { process.exitCode = code ?? 1; });
}
