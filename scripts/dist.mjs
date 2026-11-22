import { spawn } from 'node:child_process';
import { copyFile, mkdir, mkdtemp, readFile, rename, rm, stat } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const projectRoot = fileURLToPath(new URL('../', import.meta.url));

function run(command, args, options) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { ...options, stdio: 'inherit', windowsHide: true });
    child.once('error', reject);
    child.once('exit', (code, signal) => {
      if (code === 0) resolve();
      else reject(new Error(`${command} 退出失败（${signal || code}）`));
    });
  });
}

export async function buildPortable({ root = projectRoot, execute = run, env = process.env, platform = process.platform } = {}) {
  if (platform !== 'win32') throw new Error('Windows 便携包需要在 Windows 上构建');
  const config = JSON.parse(await readFile(path.join(root, 'src-tauri/tauri.conf.json'), 'utf8'));
  const pkg = JSON.parse(await readFile(path.join(root, 'package.json'), 'utf8'));
  const { productName, version } = config;
  if (typeof productName !== 'string' || !/^[a-z0-9][a-z0-9._-]*$/i.test(productName)
      || typeof version !== 'string' || !/^[0-9][0-9a-z.+-]*$/i.test(version)) {
    throw new Error('便携包名称或版本无效，请检查 tauri.conf.json');
  }
  if (pkg.version !== version) throw new Error('package.json 与 tauri.conf.json 的版本不一致');

  const targetDir = path.resolve(root, 'src-tauri', env.CARGO_TARGET_DIR || 'target');
  const target = env.CARGO_BUILD_TARGET ? path.basename(env.CARGO_BUILD_TARGET, '.json') : '';
  const releaseDir = path.join(targetDir, target, 'release');
  const buildEnv = { ...env, CARGO_TARGET_DIR: targetDir };
  await execute(process.execPath, [path.join(root, 'scripts/tauri.mjs'), 'build', '--no-bundle'], { cwd: root, env: buildEnv });
  const executable = path.join(releaseDir, 'autobiubiu.exe');
  if (!(await stat(executable)).isFile()) throw new Error(`构建未生成可执行文件：${executable}`);

  const archive = path.join(releaseDir, `${productName}_${version}_portable.zip`);
  const temporary = await mkdtemp(path.join(releaseDir, '.portable-'));
  try {
    const content = path.join(temporary, 'content');
    const folder = path.join(content, productName);
    await mkdir(folder, { recursive: true });
    // 只复制 EXE，避免发布同目录的活动记录、配置和日志。
    await copyFile(executable, path.join(folder, `${productName}.exe`));
    const candidate = path.join(temporary, 'portable.zip');
    await execute('powershell.exe', ['-NoProfile', '-NonInteractive', '-File', path.join(root, 'scripts/package-portable.ps1'),
      '-Source', content, '-Destination', candidate, '-EntryName', `${productName}/${productName}.exe`], { cwd: root, env: buildEnv });
    const result = await stat(candidate);
    if (!result.isFile() || result.size === 0) throw new Error('便携包生成失败：ZIP 为空');
    await rename(candidate, archive);
    return archive;
  } finally {
    await rm(temporary, { recursive: true, force: true });
  }
}

async function main() {
  if (process.argv.length > 2) throw new Error('dist 不接受额外参数；安装包请使用 npm run dist:nsis');
  const archive = await buildPortable();
  console.log(`便携包已生成：${archive}`);
}

if (process.argv[1] && pathToFileURL(path.resolve(process.argv[1])).href === import.meta.url) {
  main().catch((error) => {
    console.error(`便携包打包失败：${error.message}`);
    process.exitCode = 1;
  });
}
