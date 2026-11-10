<script lang="ts">
  import { onDestroy, untrack } from 'svelte';
  import Icon from './Icon.svelte';
  import { detectClientInstallations, pickClientDirectory } from './ipc';
  import { errorMessage } from './format';
  import type { ClientInstallation, ClientSettings } from './types';

  let { settings, locked, lockReason = '', saving, onsave }: {
    settings?: ClientSettings;
    locked: boolean;
    lockReason?: string;
    saving: boolean;
    onsave: (settings: ClientSettings) => Promise<boolean>;
  } = $props();

  let executablePath = $state(untrack(() => settings?.executablePath ?? ''));
  let debugPort = $state<number | undefined>(untrack(() => settings?.debugPort ?? 9222));
  let savedKey = $state(untrack(() => JSON.stringify(settings)));
  let installations = $state<ClientInstallation[]>([]);
  let working = $state<'detect' | 'browse' | null>(null);
  let message = $state('');
  let error = $state('');
  let alive = true;
  onDestroy(() => { alive = false; });

  const portValid = $derived(Number.isInteger(debugPort) && (debugPort ?? 0) >= 1 && (debugPort ?? 0) <= 65535);
  const disabled = $derived(locked || Boolean(lockReason) || working !== null || saving);
  const dirty = $derived(executablePath !== (settings?.executablePath ?? '') || debugPort !== (settings?.debugPort ?? 9222));

  $effect(() => {
    const key = JSON.stringify(settings);
    // 定时快照带回相同配置时，保留用户正在编辑的草稿。
    if (key !== savedKey) {
      savedKey = key;
      executablePath = settings?.executablePath ?? '';
      debugPort = settings?.debugPort ?? 9222;
    }
  });

  function edited(): void { message = ''; error = ''; }
  async function choose(kind: 'detect' | 'browse'): Promise<void> {
    if (disabled) return;
    working = kind;
    edited();
    try {
      if (kind === 'browse') {
        const directory = await pickClientDirectory();
        if (alive && !locked && directory) executablePath = directory;
      } else {
        const found = await detectClientInstallations();
        if (!alive) return;
        installations = found;
        if (!locked && found[0]) executablePath = found[0].executablePath;
        message = found.length ? `识别到 ${found.length} 处安装，请保存后连接。` : '未识别到安装位置，请浏览目录或填写 biubiu.exe 的完整路径。';
      }
    } catch (cause: unknown) { if (alive) error = errorMessage(cause); }
    finally { if (alive) working = null; }
  }
  async function save(): Promise<void> {
    if (disabled || !dirty || !portValid || !executablePath.trim()) return;
    edited();
    if (await onsave({ executablePath: executablePath.trim(), debugPort: debugPort! })) {
      if (alive) message = '设置已保存，点击右上角“连接客户端”使用新配置。';
    } else if (alive) error = '设置尚未确认保存，请查看上方错误提示。';
  }
</script>

<h2 class="settings-section-title">biubiu 客户端</h2>
<form class="client-settings-panel" novalidate onsubmit={(event) => { event.preventDefault(); void save(); }}>
  <label class="field" for="client-path">运行目录或 biubiu.exe 路径</label>
  <div class="client-path-row">
    <input id="client-path" bind:value={executablePath} oninput={edited} disabled={disabled}
      placeholder="选择包含 biubiu.exe 的安装目录" spellcheck="false" aria-describedby="client-path-help" />
    <button type="button" class="secondary" disabled={disabled} onclick={() => choose('browse')}><Icon name="folder" size={15} />浏览目录</button>
    <button type="button" class="secondary" disabled={disabled} onclick={() => choose('detect')}><Icon name="refresh" size={15} />{working === 'detect' ? '正在识别…' : '重新识别'}</button>
  </div>
  <p id="client-path-help" class="muted">首次使用自动查找 Windows 注册表、安装列表和常见目录；保存时会检查 biubiu.exe 是否存在。</p>
  {#if installations.length}
    <label class="field" for="client-installations">识别到的安装位置</label>
    <select id="client-installations" class="full" disabled={disabled} value={executablePath}
      onchange={(event) => { executablePath = event.currentTarget.value; edited(); }}>
      {#each installations as installation}<option value={installation.executablePath}>{installation.source} · {installation.executablePath}</option>{/each}
    </select>
  {/if}
  <label class="field" for="client-port">本地调试端口</label>
  <div class="client-port-row">
    <input id="client-port" type="number" min="1" max="65535" step="1" bind:value={debugPort} oninput={edited}
      disabled={disabled} aria-invalid={!portValid} aria-describedby="client-port-help" />
    <span class="muted">仅连接 127.0.0.1{portValid ? `:${debugPort}` : ''}</span>
  </div>
  <p id="client-port-help" class:invalid={!portValid} class="muted">{portValid ? '默认 9222。保存配置不启动客户端，也不自动重新连接。' : '端口必须是 1–65535 之间的整数。'}</p>
  <p class="muted">需要重启已运行的客户端来开启调试时，会先确认；确认重启会中断当前加速。</p>
  {#if lockReason}<p class="client-setting-notice" role="status">{lockReason}</p>{/if}
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  <div class="client-settings-actions">
    <button type="submit" class="primary" disabled={disabled || !dirty || !portValid || !executablePath.trim()}>{saving ? '正在保存…' : '保存客户端设置'}</button>
    <span class="muted" role="status">{message || (dirty ? '有尚未保存的修改' : executablePath ? '已加载本机配置' : '尚未识别到客户端，请选择安装目录')}</span>
  </div>
</form>

<style>
  .client-settings-panel { padding: 18px 20px; border: 1px solid var(--line); border-radius: 7px; background: var(--panel); }
  .client-settings-panel > .field:first-child { margin-top: 0; }
  .client-path-row { display: flex; gap: 8px; }
  .client-path-row input { flex: 1; width: 0; }
  .client-path-row button { flex-shrink: 0; }
  .client-port-row, .client-settings-actions { display: flex; align-items: center; flex-wrap: wrap; gap: 12px; }
  .client-port-row input { width: 120px; }
  .client-settings-panel p { font-size: 11px; line-height: 1.8; margin-top: 7px; }
  .client-port-row span, .client-settings-actions span { font-size: 11px; overflow-wrap: anywhere; }
  .client-settings-actions { margin-top: 16px; }
  .client-setting-notice { color: var(--warning); }
  .invalid { color: var(--danger); }
  @media (max-width: 1000px) { .client-path-row { flex-wrap: wrap; } .client-path-row input { flex-basis: 100%; width: 100%; } }
</style>
