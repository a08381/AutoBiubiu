<script lang="ts">
  import { onMount } from 'svelte';
  import Icon from './Icon.svelte';
  import { formatMembershipTime, formatTime, isoTime } from './format';
  import type { Membership } from './types';

  let { membership, connected, loggedIn, locked, refreshing, onrefresh }: {
    membership?: Membership; connected: boolean; loggedIn: boolean;
    locked: boolean; refreshing: boolean; onrefresh: () => void;
  } = $props();
  let now = $state(Date.now());
  const expiresAt = $derived(membership?.expiresAt);
  const expired = $derived(expiresAt != null && expiresAt <= now);
  const label = $derived(membership?.kind === 'trial' ? '试用会员到期' : '会员到期');
  const source = $derived(membership?.checkedAt
    ? `客户端数据，读取于 ${formatTime(membership.checkedAt)}（北京时间）。切换账号或获得会员奖励后可刷新。`
    : '连接客户端后读取会员到期时间，不根据奖励天数推算。');

  onMount(() => {
    const timer = window.setInterval(() => { now = Date.now(); }, 1000);
    return () => window.clearInterval(timer);
  });
</script>

<div class="membership-info" title={source}>
  <Icon name="clock" size={13} /><span>{label}</span>
  {#if !connected}
    <span class="membership-placeholder">连接客户端后查看</span>
  {:else if !loggedIn}
    <span class="membership-placeholder">请先登录 biubiu</span>
  {:else if expiresAt != null}
    <time class="membership-expiry" datetime={isoTime(expiresAt)}>{formatMembershipTime(expiresAt)}</time>
    <span class="membership-zone">北京时间</span>
    {#if expired}<span class="tag warn">已到期</span>{/if}
  {:else}
    <span class="membership-placeholder">客户端未提供到期时间</span>
  {/if}
  <button type="button" class="icon-button membership-refresh" aria-label="刷新会员到期时间"
    title={connected ? '让客户端更新会员资料并重新读取；任务运行中请等待结束' : '请先连接客户端'}
    disabled={locked || !connected} aria-busy={refreshing} onclick={onrefresh}>
    <Icon name="refresh" size={13} />
  </button>
</div>
