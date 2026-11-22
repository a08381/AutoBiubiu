import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { copyFile, mkdir, mkdtemp, readFile, readdir, rm, stat, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { buildPortable } from '../scripts/dist.mjs';

const freshExe = Buffer.from('MZ-current-build');
const oldZip = Buffer.from('previous portable ZIP');
const realArchiver = new URL('../scripts/package-portable.ps1', import.meta.url);

async function fixture(t) {
  const root = await mkdtemp(path.join(tmpdir(), 'AutoBiubiu portable '));
  t.after(() => rm(root, { recursive: true, force: true }));
  const release = path.join(root, 'src-tauri/target/release');
  const archive = path.join(release, 'AutoBiubiu_0.3.1_portable.zip');
  await mkdir(path.join(release, 'data'), { recursive: true });
  await mkdir(path.join(root, 'scripts'));
  await writeFile(path.join(root, 'package.json'), JSON.stringify({ version: '0.3.1' }));
  await writeFile(path.join(root, 'src-tauri/tauri.conf.json'), JSON.stringify({ productName: 'AutoBiubiu', version: '0.3.1' }));
  await writeFile(path.join(release, 'autobiubiu.exe'), 'MZ-stale-build');
  await writeFile(path.join(release, 'data/settings.json'), 'PRIVATE-FIXTURE-DO-NOT-PACK');
  await writeFile(archive, oldZip);
  await copyFile(realArchiver, path.join(root, 'scripts/package-portable.ps1'));
  return { root, release, archive };
}

function runner({ onBuild, onArchive } = {}) {
  const calls = [];
  const execute = async (command, args, options) => {
    calls.push({ command, args, options });
    if (command === process.execPath) {
      assert.deepEqual(args, [path.join(options.cwd, 'scripts/tauri.mjs'), 'build', '--no-bundle']);
      if (onBuild) return onBuild(args, options);
      const target = options.env.CARGO_BUILD_TARGET ? path.basename(options.env.CARGO_BUILD_TARGET, '.json') : '';
      const release = path.join(options.env.CARGO_TARGET_DIR, target, 'release');
      await mkdir(release, { recursive: true });
      await writeFile(path.join(release, 'autobiubiu.exe'), freshExe);
      return;
    }
    assert.equal(command, 'powershell.exe');
    if (onArchive) return onArchive(command, args, options);
    const source = args[args.indexOf('-Source') + 1];
    const destination = args[args.indexOf('-Destination') + 1];
    const entryName = args[args.indexOf('-EntryName') + 1];
    assert.deepEqual(await readdir(source), ['AutoBiubiu']);
    assert.deepEqual(await readdir(path.join(source, 'AutoBiubiu')), ['AutoBiubiu.exe']);
    const bytes = await readFile(path.join(source, entryName));
    assert.deepEqual(bytes, freshExe);
    await writeFile(destination, JSON.stringify({ entryName, executable: bytes.toString('utf8') }));
  };
  return { execute, calls };
}

function options(root, execute, extra = {}) {
  return { root, execute, env: {}, platform: 'win32', ...extra };
}

async function noTemporaryFiles(release) {
  assert.ok((await readdir(release)).every(name => !name.startsWith('.portable-')));
}

test('npm dist produces portable ZIPs and preserves the NSIS command separately', async () => {
  const pkg = JSON.parse(await readFile(new URL('../package.json', import.meta.url), 'utf8'));
  assert.equal(pkg.scripts.dist, 'node scripts/dist.mjs');
  assert.equal(pkg.scripts['dist:nsis'], 'node scripts/tauri.mjs build --bundles nsis');
  assert.equal(pkg.scripts.pack, 'node scripts/tauri.mjs build --no-bundle');
  assert.match(pkg.scripts.test, /test:packaging/);
});

test('builds first, reproduces the single-EXE folder layout, and excludes local data', async t => {
  const { root, release, archive } = await fixture(t);
  const { execute, calls } = runner();
  assert.equal(await buildPortable(options(root, execute)), archive);
  assert.equal(calls.length, 2);
  assert.equal(calls[0].command, process.execPath);
  assert.equal(calls[1].command, 'powershell.exe');
  assert.deepEqual(JSON.parse(await readFile(archive, 'utf8')), { entryName: 'AutoBiubiu/AutoBiubiu.exe', executable: freshExe.toString('utf8') });
  assert.equal(await readFile(path.join(release, 'data/settings.json'), 'utf8'), 'PRIVATE-FIXTURE-DO-NOT-PACK');
  await noTemporaryFiles(release);
});

test('uses the configured version rather than hardcoding the old archive name', async t => {
  const { root, release } = await fixture(t);
  await writeFile(path.join(root, 'package.json'), JSON.stringify({ version: '0.4.2-beta.1' }));
  await writeFile(path.join(root, 'src-tauri/tauri.conf.json'), JSON.stringify({ productName: 'AutoBiubiu', version: '0.4.2-beta.1' }));
  const { execute } = runner();
  assert.equal(await buildPortable(options(root, execute)), path.join(release, 'AutoBiubiu_0.4.2-beta.1_portable.zip'));
});

test('respects an explicit Cargo output directory and target without packing a stale default EXE', async t => {
  const { root, release } = await fixture(t);
  const { execute } = runner();
  const env = { CARGO_TARGET_DIR: 'custom target', CARGO_BUILD_TARGET: 'x86_64-pc-windows-msvc' };
  const archive = await buildPortable(options(root, execute, { env }));
  assert.equal(archive, path.join(root, 'src-tauri/custom target/x86_64-pc-windows-msvc/release/AutoBiubiu_0.3.1_portable.zip'));
  assert.deepEqual(await readFile(path.join(release, 'AutoBiubiu_0.3.1_portable.zip')), oldZip);
});

test('a failed build never packs the existing EXE or overwrites the previous ZIP', async t => {
  const { root, release, archive } = await fixture(t);
  const { execute, calls } = runner({ onBuild: () => { throw new Error('fixture build failed'); } });
  await assert.rejects(buildPortable(options(root, execute)), /fixture build failed/);
  assert.equal(calls.length, 1);
  assert.deepEqual(await readFile(archive), oldZip);
  await noTemporaryFiles(release);
});

test('a missing build artifact does not overwrite the previous ZIP', async t => {
  const { root, archive } = await fixture(t);
  const { execute } = runner({ onBuild: async (_args, { env }) => rm(path.join(env.CARGO_TARGET_DIR, 'release/autobiubiu.exe')) });
  await assert.rejects(buildPortable(options(root, execute)), { code: 'ENOENT' });
  assert.deepEqual(await readFile(archive), oldZip);
});

test('failed compression preserves the previous ZIP and removes its own staging files', async t => {
  const { root, release, archive } = await fixture(t);
  const { execute } = runner({ onArchive: () => { throw new Error('fixture compression failed'); } });
  await assert.rejects(buildPortable(options(root, execute)), /fixture compression failed/);
  assert.deepEqual(await readFile(archive), oldZip);
  await noTemporaryFiles(release);
});

test('empty compression output is rejected without replacing the previous ZIP', async t => {
  const { root, release, archive } = await fixture(t);
  const { execute } = runner({ onArchive: async (_command, args) => writeFile(args[args.indexOf('-Destination') + 1], '') });
  await assert.rejects(buildPortable(options(root, execute)), /ZIP 为空/);
  assert.deepEqual(await readFile(archive), oldZip);
  await noTemporaryFiles(release);
});

test('mismatched versions and unsafe file names fail before building', async t => {
  const { root } = await fixture(t);
  const { execute, calls } = runner();
  const config = path.join(root, 'src-tauri/tauri.conf.json');
  await writeFile(config, JSON.stringify({ productName: 'AutoBiubiu', version: '0.4.0' }));
  await assert.rejects(buildPortable(options(root, execute)), /版本不一致/);
  await writeFile(config, JSON.stringify({ productName: '../AutoBiubiu', version: '0.3.1' }));
  await assert.rejects(buildPortable(options(root, execute)), /名称或版本无效/);
  assert.equal(calls.length, 0);
});

test('non-Windows execution fails before invoking the build toolchain', async () => {
  const { execute, calls } = runner();
  await assert.rejects(buildPortable(options('/unused', execute, { platform: 'linux' })), /需要在 Windows/);
  assert.equal(calls.length, 0);
});

function powershell(command, args, options) {
  const result = spawnSync(command, args, { ...options, encoding: 'utf8', timeout: 30000 });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(result.stderr || `PowerShell exit ${result.status}`);
}

test('Windows archiver produces and hash-verifies a real single-entry ZIP', { skip: process.platform !== 'win32' }, async t => {
  const { root, release, archive } = await fixture(t);
  const { execute } = runner({ onArchive: powershell });
  await buildPortable(options(root, execute, { env: { ...process.env, CARGO_TARGET_DIR: '', CARGO_BUILD_TARGET: '' } }));
  const bytes = await readFile(archive);
  assert.equal(bytes.subarray(0, 2).toString('ascii'), 'PK');
  assert.ok((await stat(archive)).size > freshExe.length);
  await noTemporaryFiles(release);
});

test('Windows archiver rejects an extra staged data file and keeps the old ZIP', { skip: process.platform !== 'win32' }, async t => {
  const { root, release, archive } = await fixture(t);
  const { execute } = runner({ onArchive: async (command, args, settings) => {
    const source = args[args.indexOf('-Source') + 1];
    await writeFile(path.join(source, 'AutoBiubiu/private-fixture.json'), 'DO-NOT-PACK');
    powershell(command, args, settings);
  } });
  await assert.rejects(buildPortable(options(root, execute, { env: { ...process.env, CARGO_TARGET_DIR: '', CARGO_BUILD_TARGET: '' } })), /Unexpected portable ZIP contents/);
  assert.deepEqual(await readFile(archive), oldZip);
  await noTemporaryFiles(release);
});
