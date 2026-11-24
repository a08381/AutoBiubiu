<script lang="ts">
  import Icon from './Icon.svelte';
  import { errorMessage } from './format';
  import type { ReleaseUpdate } from './types';

  let { update, onopen, onclose }: {
    update: ReleaseUpdate; onopen: () => Promise<void>; onclose: () => void;
  } = $props();
  let opening = $state(false);
  let error = $state('');

  async function open(): Promise<void> {
    if (opening) return;
    opening = true;
    error = '';
    try { await onopen(); }
    catch (cause: unknown) { error = `打开更新页面失败：${errorMessage(cause)}`; }
    finally { opening = false; }
  }
</script>

<section class="update-banner" role="status" aria-label="应用更新提示">
  <Icon name="gift" size={20} />
  <div class="update-copy">
    <strong>发现新版本 v{update.latestVersion}</strong><span>当前 v{update.currentVersion}，可前往 Release 页面下载更新。</span>
    {#if error}<p class="error" role="alert">{error}</p>{/if}
  </div>
  <div class="update-actions">
    <button type="button" class="secondary" disabled={opening} onclick={open}><Icon name="arrow" size={15} />{opening ? '正在打开…' : '查看更新'}</button>
    <button type="button" class="icon-button" aria-label="暂时关闭更新提示" onclick={onclose}><Icon name="close" size={15} /></button>
  </div>
</section>
