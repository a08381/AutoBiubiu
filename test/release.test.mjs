import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { compareVersions, createGitHubClient, generateReleaseNotes, parseVersion, planRelease, publishRelease, readCommits } from '../scripts/release.mjs';

function git(root, ...args) {
  return execFileSync('git', args, { cwd: root, encoding: 'utf8', windowsHide: true }).trim();
}

async function fixture(t) {
  const root = await mkdtemp(path.join(tmpdir(), 'AutoBiubiu release '));
  t.after(() => rm(root, { recursive: true, force: true }));
  await mkdir(path.join(root, 'src-tauri'));
  // 测试样本固定为 0.3.2，项目下一次升版时无需修改测试期望。
  const version = '0.3.2';
  await writeFile(path.join(root, 'package.json'), JSON.stringify({ version }));
  await writeFile(path.join(root, 'package-lock.json'), JSON.stringify({ version, packages: { '': { version } } }));
  await writeFile(path.join(root, 'src-tauri/tauri.conf.json'), JSON.stringify({ version, productName: 'AutoBiubiu' }));
  await writeFile(path.join(root, 'src-tauri/Cargo.toml'), `[package]\nname = "autobiubiu"\nversion = "${version}"\n`);
  await writeFile(path.join(root, 'src-tauri/Cargo.lock'), `version = 4\n\n[[package]]\nname = "autobiubiu"\nversion = "${version}"\n`);
  git(root, 'init', '--initial-branch=main');
  git(root, 'config', 'core.autocrlf', 'false');
  git(root, 'config', 'user.name', 'Release fixture');
  git(root, 'config', 'user.email', 'fixture@example.invalid');
  git(root, 'add', '.');
  git(root, 'commit', '-m', '初始化项目');
  git(root, 'tag', 'v0.3.1');
  git(root, 'commit', '--allow-empty', '-m', 'feat：增加更新横幅', '-m', '界面显示最新版本。');
  git(root, 'commit', '--allow-empty', '-m', 'fix(tasks): 修复任务计时');
  git(root, 'commit', '--allow-empty', '-m', 'chore：将版本号升至 0.3.2');
  return { root, repository: 'owner/repo', target: 'HEAD' };
}

function releaseClient(releases) {
  return { request: async (method, route) => {
    assert.equal(method, 'GET');
    assert.match(route, /^repos\/owner\/repo\/releases\?per_page=100&page=\d+$/);
    const page = Number(new URL(route, 'https://api.github.com/').searchParams.get('page'));
    return releases.slice((page - 1) * 100, page * 100);
  } };
}

test('compares full SemVer, including prereleases, metadata and legacy v prefixes', () => {
  assert.ok(compareVersions('0.3.10', '0.3.9') > 0);
  assert.ok(compareVersions('0.4.0', '0.4.0-rc.2') > 0);
  assert.ok(compareVersions('0.4.0-rc.10', '0.4.0-rc.2') > 0);
  assert.ok(compareVersions('0.4.0-1', '0.4.0-alpha') < 0);
  assert.ok(compareVersions('0.4.0-alpha', '0.4.0-alpha.1') < 0);
  assert.equal(compareVersions('v0.3.2+build.1', '0.3.2'), 0);
  for (const invalid of ['0.3', '01.3.2', '0.3.2-01', 'latest', null]) assert.equal(parseVersion(invalid), null);
});

test('skips an existing release under either tag style, and never republishes an older version', async t => {
  const params = await fixture(t);
  for (const tag_name of ['0.3.2', 'v0.3.2', '0.3.10']) {
    const state = await planRelease({ ...params, client: releaseClient([{ tag_name, draft: false }]) });
    assert.equal(state.shouldRelease, false);
  }
});

test('a future prerelease does not suppress a stable patch release or shorten its change history', async t => {
  const params = await fixture(t);
  const state = await planRelease({
    ...params,
    client: releaseClient([
      { tag_name: '0.4.0-beta.1', draft: false, prerelease: true },
      { tag_name: '0.3.2-beta.1', draft: false, prerelease: true },
      { tag_name: 'v0.3.1', draft: false, prerelease: false },
    ]),
  });
  assert.equal(state.shouldRelease, true);
  assert.equal(state.previousTag, 'v0.3.1');
  assert.equal(state.prerelease, false);
});

test('finds the previous published release across API pages and reads the entire release range', async t => {
  const params = await fixture(t);
  const releases = [...Array.from({ length: 100 }, (_, index) => ({ tag_name: `unrelated-${index}`, draft: false })),
    { tag_name: 'v0.3.1', draft: false }];
  const state = await planRelease({ ...params, client: releaseClient(releases) });
  assert.equal(state.tag, '0.3.2');
  assert.equal(state.previousTag, 'v0.3.1');
  const commits = readCommits({ root: params.root, target: state.targetSha, previousTag: state.previousTag });
  assert.deepEqual(commits.map(commit => commit.subject), ['feat：增加更新横幅', 'fix(tasks): 修复任务计时', 'chore：将版本号升至 0.3.2']);
  assert.equal(commits[0].body, '界面显示最新版本。');
  const notes = generateReleaseNotes({ ...state, commits });
  assert.match(notes, /新增功能 1 项、问题修复 1 项、构建与发布 1 项/);
  assert.match(notes, /tasks：修复任务计时/);
  assert.match(notes, /compare\/v0\.3\.1\.\.\.0\.3\.2/);
  assert.doesNotMatch(notes, /初始化项目/);
});

test('first release covers all history and rejects mismatched version files', async t => {
  const params = await fixture(t);
  const state = await planRelease({ ...params, client: releaseClient([]) });
  assert.equal(state.previousTag, null);
  assert.match(generateReleaseNotes({ ...state, commits: readCommits({ root: params.root }) }), /首次发布，共包含 \*\*4\*\* 条提交/);
  await writeFile(path.join(params.root, 'src-tauri/tauri.conf.json'), JSON.stringify({ productName: 'AutoBiubiu', version: '0.4.0' }));
  await assert.rejects(planRelease({ ...params, client: releaseClient([]) }), /版本必须一致/);
});

test('refuses to attach a newly built binary to a version tag from another commit', async t => {
  const params = await fixture(t);
  git(params.root, 'tag', '0.3.2', 'v0.3.1');
  await assert.rejects(planRelease({ ...params, client: releaseClient([]) }), /指向另一提交/);
});

test('missing release history fails explicitly instead of silently expanding the commit range', async t => {
  const params = await fixture(t);
  await assert.rejects(planRelease({ ...params, client: releaseClient([{ tag_name: '0.3.0', draft: false }]) }));
});

test('descriptions preserve untyped commits, mark breaking changes and escape Markdown/HTML', () => {
  const notes = generateReleaseNotes({
    version: '0.4.0', repository: 'owner/repo',
    commits: [
      { sha: 'a'.repeat(40), subject: 'feat(api)!: 变更接口', body: 'BREAKING CHANGE: 需要迁移旧配置' },
      { sha: 'b'.repeat(40), subject: '<script>bad()</script> [link](evil)', body: '' },
    ],
  });
  assert.match(notes, /升级注意事项/);
  assert.match(notes, /需要迁移旧配置/);
  assert.match(notes, /其他更新/);
  assert.match(notes, /&lt;script&gt;/);
  assert.doesNotMatch(notes, /<script>|\[link\]/);
});

test('HTTP failures abort release planning instead of being treated as no releases', async () => {
  const client = createGitHubClient({
    token: 'fixture-token',
    fetchImpl: async () => new Response('rate limit', { status: 403 }),
  });
  await assert.rejects(client.request('GET', 'repos/owner/repo/releases'), /HTTP 403/);
});

async function publishingFixture(t, { failUpload = false, existingAsset = false, prerelease = false } = {}) {
  const params = await fixture(t);
  const state = await planRelease({ ...params, client: releaseClient([{ tag_name: 'v0.3.1', draft: false }]) });
  state.prerelease = prerelease;
  const bytes = Buffer.from([0x50, 0x4b, 0x03, 0x04, 1, 2, 3, 4]);
  const archive = path.join(params.root, state.assetName);
  await writeFile(archive, bytes);
  const calls = [];
  let draft = null;
  const asset = { id: 2, name: state.assetName, state: 'uploaded', size: bytes.length, digest: `sha256:${createHash('sha256').update(bytes).digest('hex')}` };
  const client = { request: async (method, route, body) => {
    calls.push({ method, route, body });
    if (method === 'GET' && route.startsWith('repos/owner/repo/releases?')) return draft ? [draft] : [];
    if (method === 'POST' && route === 'repos/owner/repo/releases') {
      assert.equal(body.draft, true);
      assert.equal(body.target_commitish, state.targetSha);
      draft = { ...body, id: 1, upload_url: 'https://uploads.github.com/repos/owner/repo/releases/1/assets{?name,label}', html_url: 'https://github.com/owner/repo/releases/tag/0.3.2' };
      return draft;
    }
    if (method === 'GET' && route.includes('/1/assets?')) return existingAsset ? [asset] : [];
    if (method === 'POST' && route.startsWith('https://uploads.github.com/')) {
      assert.deepEqual(body, bytes);
      if (failUpload) throw new Error('fixture upload failed');
      return asset;
    }
    if (method === 'PATCH' && route.endsWith('/releases/1')) {
      draft = { ...draft, ...body };
      return draft;
    }
    throw new Error(`Unexpected ${method} ${route}`);
  } };
  return { ...params, state, archive, notes: '真实提交生成的说明', client, calls, current: () => draft };
}

test('publishes only after ZIP upload succeeds, tied to the exact build commit', async t => {
  const params = await publishingFixture(t);
  const release = await publishRelease(params);
  assert.equal(release.draft, false);
  assert.equal(release.make_latest, 'true');
  assert.equal(params.calls.at(-1).body.draft, false);
  assert.ok(params.calls.findIndex(call => call.route.startsWith('https://uploads.github.com/')) < params.calls.length - 1);
});

test('upload failure leaves an unpublished draft; retry reuses the draft and an already verified asset', async t => {
  const params = await publishingFixture(t, { failUpload: true });
  await assert.rejects(publishRelease(params), /fixture upload failed/);
  assert.equal(params.current().draft, true);
  assert.equal(params.calls.filter(call => call.body?.draft === false).length, 0);
  params.client.request = ((original) => async (method, route, body) => {
    if (method === 'POST' && route.startsWith('https://uploads.github.com/')) return { state: 'uploaded', size: body.length };
    return original(method, route, body);
  })(params.client.request);
  assert.equal((await publishRelease(params)).draft, false);
  assert.equal(params.calls.filter(call => call.method === 'POST' && call.route === 'repos/owner/repo/releases').length, 1);
  const verified = await publishingFixture(t, { existingAsset: true, prerelease: true });
  assert.equal((await publishRelease(verified)).make_latest, 'false');
  assert.equal(verified.calls.filter(call => call.route.startsWith('https://uploads.github.com/')).length, 0);
});

test('an empty or mislabeled ZIP cannot create a draft release', async t => {
  const params = await publishingFixture(t);
  await writeFile(params.archive, '');
  await assert.rejects(publishRelease(params), /非空 ZIP/);
  assert.equal(params.calls.length, 0);
});
