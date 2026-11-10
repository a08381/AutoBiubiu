import { spawn } from 'node:child_process';
import { mkdir, mkdtemp, readFile, access } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import assert from 'node:assert/strict';
import { setTimeout as delay } from 'node:timers/promises';

const root = fileURLToPath(new URL('../', import.meta.url));
const executable = path.resolve(process.argv[2] || path.join(root, 'src-tauri/target/release/autobiubiu.exe'));
await access(executable);
await mkdir(path.join(root, 'data'), { recursive: true });
const outputDir = await mkdtemp(path.join(root, 'data', 'tauri-smoke-'));
const children = [];
function launch(command, args) {
  const child = spawn(command, args, { cwd: root, env: process.env, stdio: 'inherit' });
  const exited = new Promise((resolve, reject) => {
    child.once('error', reject);
    child.once('exit', (code, signal) => resolve({ code, signal }));
  });
  // 立即安装错误处理，避免等待窗口就绪期间出现未处理的拒绝。
  exited.catch(() => {});
  children.push(child);
  return { child, exited };
}
async function waitFile(name, timeout = 60000) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    try { return JSON.parse(await readFile(path.join(outputDir, name), 'utf8')); }
    catch (error) { if (error.code !== 'ENOENT' && !(error instanceof SyntaxError)) throw error; }
    if (first.child.exitCode !== null || first.child.signalCode !== null) {
      let detail = '可能已有常规实例';
      try {
        const report = JSON.parse(await readFile(path.join(outputDir, 'report.json'), 'utf8'));
        detail = report.error || JSON.stringify(report);
      } catch { /* 尚未生成报告时保留启动诊断。 */ }
      throw new Error(`桌面进程提前结束：${detail}；记录目录：${outputDir}`);
    }
    await delay(100);
  }
  throw new Error(`等待 ${name} 超时，记录目录：${outputDir}`);
}
const first = launch(executable, [`--smoke-test=${outputDir}`]);
let watchdog;
try {
  const ready = await waitFile('ready.json');
  await waitFile('ui-ready.json');
  if (process.platform === 'win32') {
    const screenshot = launch('powershell.exe', ['-NoProfile', '-File',
      path.join(root, 'scripts/capture-window.ps1'), '-ProcessId', String(ready.pid), '-OutputPath', path.join(outputDir, 'window.png')]);
    const captured = await screenshot.exited;
    assert.equal(captured.code, 0, '窗口截图失败');
  }
  const second = launch(executable, [`--smoke-test=${outputDir}`]);
  const result = await Promise.race([
    first.exited,
    new Promise((_, reject) => { watchdog = setTimeout(() => reject(new Error('桌面冒烟测试超过 60 秒')), 60000); }),
  ]);
  assert.equal(result.code, 0, `桌面进程异常退出，记录目录：${outputDir}`);
  const secondResult = await second.exited;
  assert.equal(secondResult.code, 0, '第二实例未正常退出');
  const report = JSON.parse(await readFile(path.join(outputDir, 'report.json'), 'utf8'));
  assert.equal(report.passed, true, JSON.stringify(report));
  assert.equal(report.shutdown, true);
  assert.equal(report.checks.singleInstance, true);
  if (report.checks.clientSettings) {
    const settings = JSON.parse(await readFile(path.join(outputDir, 'profile/data/settings.json'), 'utf8'));
    assert.equal(path.resolve(settings.executablePath), path.join(outputDir, 'client-fixture/biubiu.exe'));
    assert.ok([9333, 9334].includes(settings.debugPort), '界面保存的端口未写入隔离配置');
  }
  console.log(`Tauri 桌面冒烟通过：${Object.keys(report.checks).join('、')}、退出保存`);
  console.log(`报告与截图：${outputDir}`);
} finally {
  clearTimeout(watchdog);
  // 只回收由本测试创建的子进程，不终止用户已在运行的桌面实例。
  for (const child of children) if (child.exitCode === null && child.signalCode === null) child.kill();
}
