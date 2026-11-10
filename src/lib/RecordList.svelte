<script lang="ts">
  import { formatTime, isoTime, prettyJson } from './format';
  import type { PrizeRecord, RunLog } from './types';
  import Icon from './Icon.svelte';

  let { kind, logs = [], rewards = [], compact = false }: {
    kind: 'logs' | 'rewards'; logs?: RunLog[]; rewards?: PrizeRecord[]; compact?: boolean;
  } = $props();
</script>

<div class="record-list" class:compact>
  {#if kind === 'logs'}
    {#if !logs.length}
      <div class="empty-state"><span class="empty-icon"><Icon name="logs" size={26} /></span><h3>暂无执行记录</h3><p>任务开始后，运行消息会显示在这里。</p></div>
    {:else}
      <ol class="logs" aria-label="执行日志，最新记录在前">
        {#each logs as item}
          <li class="log" class:warn={item.level === 'warn'} class:error={item.level === 'error'}>
            <time datetime={isoTime(item.time)} title={formatTime(item.time)}>{formatTime(item.time, 'clock')}</time>
            <span class="log-level">{item.level === 'error' ? '错误' : item.level === 'warn' ? '提醒' : '记录'}</span><span class="log-message">{item.message}</span>
          </li>
        {/each}
      </ol>
    {/if}
  {:else}
    {#if !rewards.length}
      <div class="empty-state"><span class="empty-icon"><Icon name="gift" size={30} /></span><h3>奖励在路上</h3><p>完成抽奖后，奖品与返回详情会保存在本机。</p></div>
    {/if}
    {#each rewards as item}
      <article class="reward"><span class="reward-icon"><Icon name="gift" size={20} /></span><div>
        <h3>{item.prizeName || '抽奖结果已记录'}</h3>
        <p>{item.activityName || item.activityId || '活动'} · <time datetime={isoTime(item.time)}>{formatTime(item.time)}</time></p>
        <details><summary>查看返回详情</summary>
          {#if item.inventoryDetailId}<p>奖品记录编号：{item.inventoryDetailId}</p>{/if}
          <pre>{prettyJson(item.detail ?? item.result)}</pre>
        </details>
      </div></article>
    {/each}
  {/if}
</div>
