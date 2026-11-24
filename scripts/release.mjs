import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { appendFile, readFile, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const projectRoot = fileURLToPath(new URL('../', import.meta.url));
const categories = [
  ['feat', '新增功能'], ['fix', '问题修复'], ['perf', '性能优化'],
  ['refactor', '代码改进'], ['docs', '文档更新'],
  ['build', '构建与发布'], ['test', '测试'], ['other', '其他更新'],
];

export function parseVersion(value) {
  if (typeof value !== 'string') return null;
  const match = value.match(/^v?(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([\da-zA-Z-]+(?:\.[\da-zA-Z-]+)*))?(?:\+([\da-zA-Z-]+(?:\.[\da-zA-Z-]+)*))?$/);
  if (!match) return null;
  const prerelease = match[4]?.split('.') || [];
  if (prerelease.some(part => /^\d+$/.test(part) && part.length > 1 && part[0] === '0')) return null;
  return { core: match.slice(1, 4).map(BigInt), prerelease };
}

export function compareVersions(left, right) {
  const a = parseVersion(left);
  const b = parseVersion(right);
  if (!a || !b) throw new Error(`无效的语义版本：${left} / ${right}`);
  for (let i = 0; i < 3; i++) {
    if (a.core[i] !== b.core[i]) return a.core[i] > b.core[i] ? 1 : -1;
  }
  if (!a.prerelease.length || !b.prerelease.length) {
    return Number(!a.prerelease.length) - Number(!b.prerelease.length);
  }
  for (let i = 0; i < Math.max(a.prerelease.length, b.prerelease.length); i++) {
    const x = a.prerelease[i];
    const y = b.prerelease[i];
    if (x === undefined || y === undefined) return Number(y === undefined) - Number(x === undefined);
    if (x === y) continue;
    const xn = /^\d+$/.test(x);
    const yn = /^\d+$/.test(y);
    if (xn && yn) return BigInt(x) > BigInt(y) ? 1 : -1;
    if (xn !== yn) return xn ? -1 : 1;
    return x > y ? 1 : -1;
  }
  return 0;
}

function git(root, args) {
  return execFileSync('git', args, { cwd: root, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'], maxBuffer: 32 * 1024 * 1024, windowsHide: true }).trimEnd();
}

function resolveCommit(root, ref) {
  return git(root, ['rev-parse', '--verify', `${ref}^{commit}`]);
}

export async function readReleaseVersion(root = projectRoot) {
  const sources = await Promise.all(['package.json', 'package-lock.json', 'src-tauri/tauri.conf.json', 'src-tauri/Cargo.toml', 'src-tauri/Cargo.lock']
    .map(file => readFile(path.join(root, file), 'utf8')));
  const pkg = JSON.parse(sources[0]);
  const lock = JSON.parse(sources[1]);
  const config = JSON.parse(sources[2]);
  const cargoPackage = sources[3].match(/^\[package\]\s*([\s\S]*?)(?=^\[|(?![\s\S]))/m)?.[1];
  const cargoVersion = cargoPackage?.match(/^version\s*=\s*"([^"]+)"/m)?.[1];
  const cargoLockVersion = sources[4].split('[[package]]')
    .find(section => /^name\s*=\s*"autobiubiu"\s*$/m.test(section))?.match(/^version\s*=\s*"([^"]+)"/m)?.[1];
  const versions = [pkg.version, lock.version, lock.packages?.['']?.version, config.version, cargoVersion, cargoLockVersion];
  if (!parseVersion(pkg.version) || pkg.version.startsWith('v') || versions.some(version => version !== pkg.version)) {
    throw new Error(`应用及锁文件的版本必须一致且为有效 SemVer：${JSON.stringify(versions)}`);
  }
  if (typeof config.productName !== 'string' || !/^[a-z0-9][a-z0-9._-]*$/i.test(config.productName)) {
    throw new Error('无效的 portable 产品名称');
  }
  return { version: pkg.version, productName: config.productName };
}

export function createGitHubClient({ token, apiUrl = 'https://api.github.com', fetchImpl = fetch }) {
  if (!token) throw new Error('缺少 GH_TOKEN / GITHUB_TOKEN；工作流使用内置 github.token');
  return {
    async request(method, route, body, extraHeaders = {}) {
      const binary = Buffer.isBuffer(body);
      const response = await fetchImpl(new URL(route, `${apiUrl.replace(/\/$/, '')}/`), {
        method,
        headers: {
          Accept: 'application/vnd.github+json',
          Authorization: `Bearer ${token}`,
          'X-GitHub-Api-Version': '2026-03-10',
          'User-Agent': 'AutoBiubiu-release',
          ...(body === undefined ? {} : { 'Content-Type': binary ? 'application/zip' : 'application/json' }),
          ...extraHeaders,
        },
        body: body === undefined ? undefined : binary ? body : JSON.stringify(body),
        signal: AbortSignal.timeout(binary ? 180_000 : 60_000),
      });
      const text = await response.text();
      if (!response.ok) throw new Error(`GitHub API ${method} ${new URL(response.url || route, apiUrl).pathname}: HTTP ${response.status} ${text.slice(0, 1000)}`);
      return text ? JSON.parse(text) : null;
    },
  };
}

async function listAll(client, route) {
  const result = [];
  for (let page = 1; ; page++) {
    const entries = await client.request('GET', `${route}?per_page=100&page=${page}`);
    if (!Array.isArray(entries)) throw new Error(`GitHub API 列表响应无效：${route}`);
    result.push(...entries);
    if (entries.length < 100) return result;
  }
}

function repositoryRoute(repository) {
  if (!/^[\w.-]+\/[\w.-]+$/.test(repository)) throw new Error('repository 必须为 owner/repo');
  return `repos/${repository}`;
}

export async function planRelease({ root = projectRoot, repository, target = 'HEAD', client }) {
  const metadata = await readReleaseVersion(root);
  const prerelease = parseVersion(metadata.version).prerelease.length > 0;
  const targetSha = resolveCommit(root, target);
  const releases = await listAll(client, `${repositoryRoute(repository)}/releases`);
  // 比较版本本身，兼容历史的 0.3.2 和 v0.3.2 两种标签。
  const sameVersion = releases.filter(release => parseVersion(release.tag_name)
    && compareVersions(release.tag_name, metadata.version) === 0);
  if (sameVersion.some(release => !release.draft)) {
    return { shouldRelease: false, reason: `${metadata.version} 已发布`, ...metadata };
  }
  // 正式版只与正式版比较，避免未来版本的 beta 阻止当前稳定版的补丁发布。
  const published = releases.filter(release => !release.draft && parseVersion(release.tag_name)
    && (prerelease || (!release.prerelease && parseVersion(release.tag_name).prerelease.length === 0)));
  if (published.some(release => compareVersions(release.tag_name, metadata.version) > 0)) {
    return { shouldRelease: false, reason: `${metadata.version} 低于已发布版本`, ...metadata };
  }
  const draft = sameVersion.find(release => release.draft);
  const tags = git(root, ['tag', '--list']).split('\n');
  const tag = draft?.tag_name || [metadata.version, `v${metadata.version}`].find(name => tags.includes(name)) || metadata.version;
  if (tags.includes(tag) && resolveCommit(root, `refs/tags/${tag}`) !== targetSha) {
    throw new Error(`版本标签 ${tag} 指向另一提交；请升版，或重跑该标签对应提交的工作流`);
  }
  const previousReleases = published.filter(release => compareVersions(release.tag_name, metadata.version) < 0)
    .sort((a, b) => compareVersions(b.tag_name, a.tag_name));
  let previousTag = null;
  for (const release of previousReleases) {
    // fetch-depth: 0 保证存在所有历史；缺标签属于配置错误，不能误生成“首次发布”说明。
    const previousSha = resolveCommit(root, `refs/tags/${release.tag_name}`);
    try {
      git(root, ['merge-base', '--is-ancestor', previousSha, targetSha]);
      previousTag = release.tag_name;
      break;
    } catch (error) {
      if (error.status !== 1) throw error;
    }
  }
  return {
    shouldRelease: true, ...metadata, repository, targetSha, tag, previousTag,
    prerelease,
    assetName: `${metadata.productName}_${metadata.version}_portable.zip`,
  };
}

export function readCommits({ root = projectRoot, target = 'HEAD', previousTag = null }) {
  const targetSha = resolveCommit(root, target);
  const range = previousTag ? `${resolveCommit(root, `refs/tags/${previousTag}`)}..${targetSha}` : targetSha;
  const output = git(root, ['log', '--reverse', '-z', '--format=%H%x00%s%x00%b', range]);
  if (!output) return [];
  const fields = output.split('\0');
  if (fields.at(-1) === '') fields.pop();
  if (fields.length % 3 !== 0) throw new Error('无法解析 Git 提交记录');
  return Array.from({ length: fields.length / 3 }, (_, index) => ({
    sha: fields[index * 3], subject: fields[index * 3 + 1], body: fields[index * 3 + 2].trim(),
  }));
}

function markdown(text) {
  return text.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
    .replace(/([\\`*_[\]#|])/g, '\\$1').replace(/\r?\n/g, ' ');
}

function describeCommit(commit) {
  const match = commit.subject.match(/^(feat|fix|perf|refactor|docs|build|ci|test|chore|style|revert)(?:\(([^)]+)\))?(!)?\s*[:：]\s*(.+)$/i);
  const type = match?.[1]?.toLowerCase();
  const category = type === 'ci' || type === 'chore' ? 'build' : type === 'style' ? 'refactor'
    : categories.some(([key]) => key === type) ? type : 'other';
  const description = match ? `${match[2] ? `${match[2]}：` : ''}${match[4]}` : commit.subject;
  const breaking = commit.body.match(/BREAKING[ -]CHANGE:\s*([\s\S]+)/i)?.[1]?.trim();
  return { ...commit, category, description, breaking: breaking || (match?.[3] ? description : null) };
}

export function generateReleaseNotes({ version, productName = 'AutoBiubiu', repository, tag = version, previousTag = null, commits }) {
  repositoryRoute(repository);
  const described = commits.map(describeCommit);
  const baseUrl = `https://github.com/${repository}`;
  const changes = categories.map(([key, title]) => ({ title, commits: described.filter(commit => commit.category === key) }))
    .filter(group => group.commits.length);
  const notes = [
    `# ${productName} ${version}`, '',
    previousTag ? `本次更新相对于 **${markdown(previousTag)}**，共包含 **${commits.length}** 条提交。`
      : `首次发布，共包含 **${commits.length}** 条提交。`,
    changes.length ? `更新内容包括：${changes.map(group => `${group.title} ${group.commits.length} 项`).join('、')}。` : '此版本没有新增提交。',
    '',
  ];
  const breaking = described.filter(commit => commit.breaking);
  if (breaking.length) notes.push('## 升级注意事项', '', ...breaking.map(commit => `- ${markdown(commit.breaking)}`), '');
  for (const group of changes) {
    notes.push(`## ${group.title}`, '', ...group.commits.map(commit =>
      `- ${markdown(commit.description)} ([${commit.sha.slice(0, 7)}](${baseUrl}/commit/${commit.sha}))`), '');
  }
  notes.push('## 下载与使用', '',
    `- 下载附件 \`${productName}_${version}_portable.zip\`，解压后运行 \`${productName}/${productName}.exe\`。`,
    '- 适用于 Windows x64，需要已安装 Microsoft Edge WebView2 Runtime。',
    '- 升级前退出旧版，替换 EXE，保留原来的 `data` 目录；便携包不包含个人配置、活动记录或日志。', '');
  if (previousTag) notes.push(`**完整变更：** [${markdown(previousTag)} → ${markdown(tag)}](${baseUrl}/compare/${encodeURIComponent(previousTag)}...${encodeURIComponent(tag)})`, '');
  return `${notes.join('\n')}\n`;
}

export async function publishRelease({ root = projectRoot, state, notes, archive, client }) {
  if (!state.shouldRelease) throw new Error('此版本无需发布');
  if (resolveCommit(root, 'HEAD') !== state.targetSha) throw new Error('构建提交与发布计划不一致');
  const metadata = await readReleaseVersion(root);
  if (metadata.version !== state.version || metadata.productName !== state.productName) throw new Error('构建版本与发布计划不一致');
  if (!notes.trim()) throw new Error('Release 描述为空');
  if (path.basename(archive) !== state.assetName) throw new Error('portable 文件名与发布版本不一致');
  const bytes = await readFile(archive);
  if (bytes.length < 4 || !bytes.subarray(0, 4).equals(Buffer.from([0x50, 0x4b, 0x03, 0x04]))) throw new Error('portable 不是有效的非空 ZIP');
  const digest = `sha256:${createHash('sha256').update(bytes).digest('hex')}`;
  const route = repositoryRoute(state.repository);
  const releases = await listAll(client, `${route}/releases`);
  const sameVersion = releases.filter(release => parseVersion(release.tag_name) && compareVersions(release.tag_name, state.version) === 0);
  if (sameVersion.some(release => !release.draft)) return sameVersion.find(release => !release.draft);
  let release = sameVersion.find(release => release.draft);
  if (release && release.tag_name !== state.tag) throw new Error('草稿标签与发布计划不一致');
  const properties = { tag_name: state.tag, target_commitish: state.targetSha, name: `${state.productName} ${state.version}`, body: notes, prerelease: state.prerelease };
  if (!release) release = await client.request('POST', `${route}/releases`, { ...properties, draft: true });
  else release = await client.request('PATCH', `${route}/releases/${release.id}`, { ...properties, draft: true });
  const assets = await listAll(client, `${route}/releases/${release.id}/assets`);
  let asset = assets.find(item => item.name === state.assetName && item.state === 'uploaded' && item.digest === digest && item.size === bytes.length);
  if (!asset) {
    // 只替换未发布草稿里的失败/旧附件；已公开的 release 在上面直接返回。
    for (const old of assets.filter(item => item.name === state.assetName)) {
      await client.request('DELETE', `${route}/releases/assets/${old.id}`);
    }
    const uploadUrl = new URL(release.upload_url.replace(/\{.*$/, ''));
    uploadUrl.searchParams.set('name', state.assetName);
    asset = await client.request('POST', uploadUrl.href, bytes, { 'Content-Length': String(bytes.length) });
  }
  if (asset.state !== 'uploaded' || asset.size !== bytes.length || (asset.digest && asset.digest !== digest)) {
    throw new Error('GitHub 附件校验失败；保留草稿，重跑工作流可继续');
  }
  return client.request('PATCH', `${route}/releases/${release.id}`, {
    draft: false, prerelease: state.prerelease, make_latest: state.prerelease ? 'false' : 'true',
  });
}

function parseArguments(args) {
  const [command, ...rest] = args;
  if (!['check', 'notes', 'publish'].includes(command)) throw new Error('用法：node scripts/release.mjs <check|notes|publish> [--state 文件] [--output 文件] [--previous-tag 标签] [--repository owner/repo] [--target SHA] [--notes 文件] [--archive ZIP]');
  const options = {};
  for (let i = 0; i < rest.length; i += 2) {
    if (!/^--(state|output|previous-tag|repository|target|notes|archive)$/.test(rest[i]) || !rest[i + 1]) throw new Error(`无效参数：${rest[i]}`);
    options[rest[i].slice(2)] = rest[i + 1];
  }
  return { command, options };
}

async function main() {
  const { command, options } = parseArguments(process.argv.slice(2));
  const repository = options.repository || process.env.GITHUB_REPOSITORY;
  if (command === 'check') {
    if (!options.state) throw new Error('check 需要 --state 文件');
    const client = createGitHubClient({ token: process.env.GH_TOKEN || process.env.GITHUB_TOKEN, apiUrl: process.env.GITHUB_API_URL });
    const state = await planRelease({ repository, target: options.target || process.env.GITHUB_SHA || 'HEAD', client });
    await writeFile(options.state, `${JSON.stringify(state, null, 2)}\n`);
    if (process.env.GITHUB_OUTPUT) await appendFile(process.env.GITHUB_OUTPUT, `should_release=${state.shouldRelease}\nversion=${state.version}\n`);
    console.log(state.shouldRelease ? `准备发布 ${state.tag}，上一版：${state.previousTag || '无（首次发布）'}` : state.reason);
    return;
  }
  const state = options.state ? JSON.parse(await readFile(options.state, 'utf8')) : {
    ...await readReleaseVersion(), repository, targetSha: options.target || 'HEAD', previousTag: options['previous-tag'] || null,
  };
  if (command === 'notes') {
    const commits = readCommits({ target: state.targetSha, previousTag: state.previousTag });
    const notes = generateReleaseNotes({ ...state, commits });
    if (options.output) await writeFile(options.output, notes);
    else console.log(notes);
    return;
  }
  if (!options.state || !options.notes) throw new Error('publish 需要 --state 和 --notes 文件');
  const client = createGitHubClient({ token: process.env.GH_TOKEN || process.env.GITHUB_TOKEN, apiUrl: process.env.GITHUB_API_URL });
  const archive = options.archive || path.join(process.env.CARGO_TARGET_DIR || path.join(projectRoot, 'src-tauri/target'), 'release', state.assetName);
  const release = await publishRelease({ state, notes: await readFile(options.notes, 'utf8'), archive, client });
  console.log(`Release 已发布：${release.html_url}`);
  if (process.env.GITHUB_STEP_SUMMARY) await appendFile(process.env.GITHUB_STEP_SUMMARY, `已发布 [${state.tag}](${release.html_url})，附件：\`${state.assetName}\`。\n`);
}

if (process.argv[1] && pathToFileURL(path.resolve(process.argv[1])).href === import.meta.url) {
  main().catch(error => { console.error(`Release 失败：${error.message}`); process.exitCode = 1; });
}
