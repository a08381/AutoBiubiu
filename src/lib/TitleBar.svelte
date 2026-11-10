<script lang="ts">
  import { onMount } from 'svelte';
  import { isTauri } from '@tauri-apps/api/core';
  import { getCurrentWindow } from '@tauri-apps/api/window';
  import Icon from './Icon.svelte';

  let { onerror }: { onerror: (error: unknown) => void } = $props();
  let maximized = $state(false);
  let desktop = $state(false);

  onMount(() => {
    desktop = isTauri();
    if (!desktop) return;
    const current = getCurrentWindow();
    let disposed = false;
    let unlisten: (() => void) | undefined;
    const update = async () => {
      try {
        const value = await current.isMaximized();
        if (!disposed) maximized = value;
      } catch (error) { if (!disposed) onerror(error); }
    };
    void update();
    void current.onResized(() => { void update(); }).then((stop) => {
      if (disposed) stop(); else unlisten = stop;
    }).catch((error: unknown) => { if (!disposed) onerror(error); });
    return () => { disposed = true; unlisten?.(); };
  });

  async function control(action: 'minimize' | 'maximize' | 'close'): Promise<void> {
    if (!desktop) return;
    try {
      const current = getCurrentWindow();
      if (action === 'minimize') await current.minimize();
      else if (action === 'maximize') await current.toggleMaximize();
      // 交给现有 CloseRequested 处理，保持关闭到托盘的行为。
      else await current.close();
    } catch (error) { onerror(error); }
  }
</script>

<header class="titlebar">
  <div class="titlebar-drag" data-tauri-drag-region>
    <span class="app-symbol"><Icon name="arrow" size={14} /></span>
    <span>AutoBiubiu</span><span class="titlebar-caption">活动助手</span>
  </div>
  <div class="window-controls">
    <button type="button" aria-label="最小化" title="最小化" disabled={!desktop} onclick={() => control('minimize')}><Icon name="minimize" size={15} /></button>
    <button type="button" aria-label={maximized ? '还原窗口' : '最大化'} title={maximized ? '还原窗口' : '最大化'} disabled={!desktop} onclick={() => control('maximize')}><Icon name={maximized ? 'restore' : 'maximize'} size={14} /></button>
    <button type="button" class="window-close" aria-label="关闭到托盘" title="关闭到托盘 · 任务继续运行" disabled={!desktop} onclick={() => control('close')}><Icon name="close" size={17} /></button>
  </div>
</header>
