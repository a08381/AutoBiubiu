<script lang="ts">
  import type { Activity, ActivityTask, GameMapping } from './types';
  import Icon from './Icon.svelte';
  import { canConfigure, formatTime, mappingDescription, taskKey, taskStatus } from './format';

  interface Props {
    activity: Activity;
    selected: boolean;
    mappings: Record<string, GameMapping>;
    locked: boolean;
    clientAvailable: boolean;
    accountVerified: boolean;
    onselect: (id: string, checked: boolean) => void;
    onmapping: (activity: Activity, task: ActivityTask) => void;
    onexternal: (url: string) => void;
    onopen: (id: string) => void;
  }

  let { activity, selected, mappings, locked, clientAvailable, accountVerified,
    onselect, onmapping, onexternal, onopen }: Props = $props();
  const accelerationTasks = $derived(activity.tasks.filter((task) => task.type === 7));
  const otherTasks = $derived(activity.tasks.filter((task) => task.type !== 7));
</script>

<article class="card" class:selected aria-labelledby={`activity-${activity.id}`}>
  <div class="detail-eyebrow"><Icon name="activity" size={13} />活动详情</div>
  <div class="card-head">
    <label class="activity-check">
      <input type="checkbox" checked={selected} disabled={locked}
        onchange={(event) => onselect(activity.id, event.currentTarget.checked)} />
      <span class="sr-only">选择 {activity.name}</span>
    </label>
    <h3 id={`activity-${activity.id}`}>{activity.name}</h3>
  </div>
  <div class="card-meta">
    <span>{activity.isOver ? '活动已结束' : activity.endTime != null && activity.endTime !== '' ? `${formatTime(activity.endTime, 'date')} 截止` : '截止时间待更新'}</span>
    <span class="tag">{accountVerified ? '' : '缓存 · '}{activity.tickets ?? 0} 次抽奖</span>
  </div>
  <p class="data-source">{accountVerified ? '缓存数据 · 本次已核对账号进度' : '公开 / 缓存数据 · 非当前账号进度'}</p>
  <div class="tasks">
    {#if activity.error}
      <p class="error">读取活动失败：{activity.error}</p>
    {/if}
    {#each accelerationTasks as task (task.id)}
      <section class="task" aria-label={task.title || '加速任务'}>
        <div class="task-title">
          <h4>{task.title || '加速任务'}</h4>
          <span class="tag" class:good={accountVerified && task.status === 3} class:warn={!task.supported}>
            {#if !accountVerified}缓存 · {/if}{taskStatus(task)}
          </span>
        </div>
        <p class="task-detail">{mappingDescription(task, mappings[taskKey(activity.id, task.id)])}</p>
        {#if task.description}
          <p class="task-detail">{task.description}</p>
        {/if}
        {#if canConfigure(task)}
          <div class="task-controls">
            <span>完成可领 {task.awardNum ?? 1} 次抽奖</span>
            <button type="button" class="text-button" disabled={locked}
              onclick={() => onmapping(activity, task)}>设置游戏</button>
          </div>
        {/if}
      </section>
    {/each}
    {#if otherTasks.length}
      <details>
        <summary>其他 {otherTasks.length} 项任务 · 查看说明</summary>
        {#each otherTasks as task (task.id)}
          <p><strong>{task.title}</strong>：{task.description || task.reason || '请在官方活动页查看要求'}</p>
        {/each}
      </details>
    {/if}
    {#if !activity.tasks.length && !activity.error}
      <p class="muted">暂无任务说明，可刷新公开内容或打开活动页查看。</p>
    {/if}
  </div>
  <div class="card-foot">
    <button type="button" class="link-button" onclick={() => onexternal(activity.url)}>独立打开活动 ↗</button>
    <button type="button" class="secondary" disabled={locked || !clientAvailable}
      title={clientAvailable ? '在 biubiu 客户端打开此活动' : '请先点击连接客户端'}
      onclick={() => onopen(activity.id)}>在客户端查看</button>
  </div>
</article>
