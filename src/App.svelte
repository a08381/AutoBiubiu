<script lang="ts">
  import { onMount } from 'svelte';
  import ActivityCard from './lib/ActivityCard.svelte';
  import Icon from './lib/Icon.svelte';
  import TitleBar from './lib/TitleBar.svelte';
  import MembershipInfo from './lib/MembershipInfo.svelte';
  import ClientSettingsPanel from './lib/ClientSettingsPanel.svelte';
  import RecordList from './lib/RecordList.svelte';
  import { filterActivities, focusedActivity } from './lib/workspace';
  import { dispatch, getState, openDataDir, openExternal, requestExit } from './lib/ipc';
  import { duration, errorMessage, formatTime, taskKey, validateActivityUrl } from './lib/format';
  import type { Activity, ActivityTask, DispatchRequest, MutationRequest, RunStatus, SearchGame, Snapshot } from './lib/types';

  type DialogName = 'mapping' | 'import' | 'reconcile';
  type UtilityAction = 'external' | 'directory' | 'exit';
  interface MappingTarget { activity: Activity; task: ActivityTask }

  const statusLabels: Record<RunStatus, string> = {
    idle: '准备就绪', starting: '正在启动任务', running: '任务执行中', stopping: '正在停止',
    stopped: '已停止', completed: '本轮执行结束', error: '需要处理', interrupted: '上次运行已中断',
  };

  let snapshot = $state<Snapshot | null>(null);
  let selectedIds = $state<string[]>([]);
  let mainError = $state('');
  let stateError = $state('');
  let busyAction = $state<DispatchRequest['action'] | null>(null);
  let stopBusy = $state(false);
  let utilityBusy = $state<UtilityAction | null>(null);
  // 窗口重载和公开导入一律视为缓存。
  // 只有显式核对账号并读回最新状态后，才标记为已核对。
  let publicView = $state(true);
  let page = $state<'activities' | 'rewards' | 'logs' | 'settings'>('activities');
  let activityQuery = $state('');
  let onlySelected = $state(false);
  let focusedId = $state<string | null>(null);
  let theme = $state<'light' | 'dark' | 'system'>('light');
  let systemDark = $state(false);
  const dark = $derived(theme === 'dark' || (theme === 'system' && systemDark));
  const pageTitles = { activities: '活动', rewards: '我的奖励', logs: '执行日志', settings: '设置' };
  const pageDescriptions = {
    activities: '整理活动，剩下的交给 AutoBiubiu。',
    rewards: '每一次收获，都好好留在这里。',
    logs: '查看本轮任务的执行过程与状态。',
    settings: '让这个小工具更合你的习惯。',
  };

  let importDialog = $state<HTMLDialogElement>();
  let importUrl = $state('');
  let importError = $state('');
  let mappingDialog = $state<HTMLDialogElement>();
  let mappingTarget = $state<MappingTarget | null>(null);
  let mappingError = $state('');
  let gameQuery = $state('');
  let games = $state<SearchGame[]>([]);
  let searched = $state(false);
  let gameId = $state('');
  let areaId = $state('');
  let minutes = $state<number | undefined>(10);
  let reconcileDialog = $state<HTMLDialogElement>();
  let reconcileChecked = $state(false);
  let reconcileError = $state('');
  let reconcileKey = $state('');

  let mounted = false;
  let stateRequest: Promise<boolean> | null = null;
  let knownIds = new Set<string>();
  let mappingEpoch = 0;

  const activities = $derived(snapshot?.activities ?? []);
  const run = $derived(snapshot?.run);
  const selected = $derived(new Set(selectedIds));
  const accountVerified = $derived(Boolean(snapshot?.authenticated) && !publicView);
  const connected = $derived(Boolean(snapshot?.connection.connected));
  const runBusy = $derived(run?.status === 'starting' || run?.status === 'running' || run?.status === 'stopping');
  const editingLocked = $derived(!snapshot || Boolean(stateError) || runBusy || stopBusy
    || Boolean(snapshot.operation) || busyAction !== null || utilityBusy === 'exit');
  const visibleActivities = $derived(filterActivities(activities, activityQuery, selected, onlySelected));
  const focused = $derived(focusedActivity(visibleActivities, focusedId));
  const allSelected = $derived(visibleActivities.length > 0 && visibleActivities.every((activity) => selected.has(activity.id)));
  const partlySelected = $derived(!allSelected && visibleActivities.some((activity) => selected.has(activity.id)));
  const selectedGroups = $derived((snapshot?.plan.groups ?? []).filter((group) =>
    group.tasks.some((task) => selected.has(task.activityId))));
  const estimatedMinutes = $derived(Math.ceil(selectedGroups.reduce((total, group) => total + Math.max(0,
    ...group.tasks.filter((task) => selected.has(task.activityId)).map((task) => task.status === 2 ? 0 : task.seconds)), 0) / 60));
  const logs = $derived([...(run?.logs ?? [])].reverse());
  const rewards = $derived([...(run?.results ?? [])].reverse());
  const currentGame = $derived(games.find((game) => String(game.id) === gameId));
  const currentMapping = $derived(mappingTarget
    ? snapshot?.mappings[taskKey(mappingTarget.activity.id, mappingTarget.task.id)] : undefined);
  const pendingActivity = $derived(activities.find((activity) => activity.id === run?.pending?.activityId));
  const pendingKey = $derived(JSON.stringify(run?.pending ?? null));
  const reconcileChanged = $derived(reconcileKey !== pendingKey);
  const backgroundError = $derived(run?.cleanupError
    ? `停止加速未确认：${run.cleanupError}` : run?.status === 'error' ? run.message || '后台任务发生错误，请查看执行日志。' : '');
  const connectionText = $derived(stateError ? '本机状态暂未同步' : !snapshot ? '正在读取本机状态' : connected
    ? snapshot.connection.loggedIn ? '客户端已连接 · 已登录' : '客户端已连接 · 请登录'
    : '尚未连接客户端');
  const runTitle = $derived(run?.status === 'running' && run.group
    ? `正在加速 · ${run.group.gameName}` : run ? statusLabels[run.status] || run.status : '读取运行状态');
  const runDetail = $derived(run?.status === 'running'
    ? `本组 ${run.group?.tasks.length ?? 0} 个任务 · 达标后等待 20 秒结算缓冲，再核对并领取`
    : run?.status === 'stopping' ? '正在等待当前请求结束并停止加速，请稍候。'
    : run?.status === 'completed' && rewards[0]?.prizeName ? `最近获得：${rewards[0].prizeName} · 详细结果已保存在本机`
    : run?.message || '选好活动，连接客户端后开始；相同游戏与区服会合并计时。');

  function report(error: unknown, dialog?: DialogName): void {
    if (!mounted) return;
    const message = errorMessage(error);
    mainError = message;
    if (dialog === 'mapping') mappingError = message;
    if (dialog === 'import') importError = message;
    if (dialog === 'reconcile') reconcileError = message;
  }

  function clearDialogError(dialog?: DialogName): void {
    if (dialog === 'mapping') mappingError = '';
    if (dialog === 'import') importError = '';
    if (dialog === 'reconcile') reconcileError = '';
  }

  function refreshState(): Promise<boolean> {
    if (!mounted) return Promise.resolve(false);
    // 定时轮询与操作后的刷新共用一个请求，避免旧响应覆盖新状态。
    if (stateRequest) return stateRequest;
    stateRequest = Promise.resolve().then(async () => {
      try {
        const next = await getState();
        if (!mounted) return false;
        const previousSelection = new Set(selectedIds);
        selectedIds = next.activities.filter((activity) => previousSelection.has(activity.id)
          || !knownIds.has(activity.id)).map((activity) => activity.id);
        knownIds = new Set(next.activities.map((activity) => activity.id));
        snapshot = next;
        stateError = '';
        return true;
      } catch (error: unknown) {
        if (mounted) stateError = `本机状态读取失败：${errorMessage(error)}`;
        return false;
      } finally {
        stateRequest = null;
      }
    });
    return stateRequest;
  }

  async function refreshAfterAction(): Promise<boolean> {
    // 操作完成前发出的读取可能仍是旧数据，先等它结束，
    // 再读取新快照，随后才解除操作控件锁定。
    if (stateRequest) await stateRequest;
    return mounted ? refreshState() : false;
  }

  function selectVisible(checked: boolean): void {
    if (editingLocked) return;
    const next = new Set(selectedIds);
    for (const activity of visibleActivities) {
      if (checked) next.add(activity.id); else next.delete(activity.id);
    }
    selectedIds = [...next];
  }

  function setTheme(value: typeof theme): void {
    theme = value;
    try { window.localStorage.setItem('autobiubiu-theme', value); } catch { /* 存储不可用时仅应用到当前窗口。 */ }
  }

  $effect(() => { document.documentElement.dataset.theme = dark ? 'dark' : 'light'; });

  onMount(() => {
    mounted = true;
    try {
      const saved = window.localStorage.getItem('autobiubiu-theme');
      if (saved === 'light' || saved === 'dark' || saved === 'system') theme = saved;
    } catch { /* 保持默认浅色主题。 */ }
    const media = window.matchMedia('(prefers-color-scheme: dark)');
    systemDark = media.matches;
    const onThemeChange = (event: MediaQueryListEvent) => { systemDark = event.matches; };
    media.addEventListener('change', onThemeChange);
    void refreshState();
    const timer = window.setInterval(() => { void refreshState(); }, 2000);
    return () => {
      mounted = false;
      mappingEpoch += 1;
      window.clearInterval(timer);
      media.removeEventListener('change', onThemeChange);
    };
  });

  async function mutate(request: MutationRequest, dialog?: DialogName): Promise<boolean> {
    if (!mounted) return false;
    if (editingLocked) {
      report('请等待当前操作完成及本机状态恢复后重试。', dialog);
      return false;
    }
    busyAction = request.action;
    mainError = '';
    clearDialogError(dialog);
    if (request.action === 'connect' || request.action === 'client-settings' || request.action === 'import' || request.action === 'scan') publicView = true;
    try {
      await dispatch(request);
      if (!mounted) return false;
      const refreshed = await refreshAfterAction();
      if (!mounted) return false;
      if (!refreshed) {
        report('操作已提交，但最新状态尚未读回。请等待状态恢复，避免重复提交。', dialog);
        return false;
      }
      if (request.action === 'scan' && !request.payload.publicOnly) publicView = false;
      return true;
    } catch (error: unknown) {
      report(error, dialog);
      return false;
    } finally {
      if (mounted) busyAction = null;
    }
  }

  async function stopTasks(): Promise<void> {
    if (!mounted || stopBusy || !runBusy || run?.status === 'stopping') return;
    stopBusy = true;
    mainError = '';
    try {
      // 停止请求独立于串行操作通道，正在执行其他操作时也能提交。
      await dispatch({ action: 'stop', payload: {} });
      if (mounted) await refreshAfterAction();
    } catch (error: unknown) {
      report(error);
    } finally {
      if (mounted) stopBusy = false;
    }
  }

  function selectActivity(id: string, checked: boolean): void {
    if (editingLocked) return;
    selectedIds = checked ? [...new Set([...selectedIds, id])] : selectedIds.filter((value) => value !== id);
  }

  function showImport(): void {
    if (editingLocked) return;
    importError = '';
    importUrl = '';
    importDialog?.showModal();
  }

  async function importActivity(): Promise<void> {
    let url: string;
    try { url = validateActivityUrl(importUrl); } catch (error: unknown) {
      report(error, 'import');
      return;
    }
    if (await mutate({ action: 'import', payload: { url } }, 'import')) {
      if (mounted) importDialog?.close();
    }
  }

  function showMapping(activity: Activity, task: ActivityTask): void {
    if (editingLocked) return;
    mappingEpoch += 1;
    mappingTarget = { activity, task };
    const mapping = snapshot?.mappings[taskKey(activity.id, task.id)];
    gameQuery = mapping?.gameName || task.gameName || '';
    minutes = (mapping?.seconds ?? task.seconds ?? 600) / 60;
    mappingError = '';
    games = [];
    searched = false;
    gameId = '';
    areaId = '';
    mappingDialog?.showModal();
    // 搜索需要显式点击，打开弹窗不会自动连接 biubiu。
  }

  async function searchGames(): Promise<void> {
    if (!mounted || !mappingTarget) return;
    if (editingLocked || !connected) {
      report('请先连接客户端，并等待当前操作完成后搜索。', 'mapping');
      return;
    }
    const keyword = gameQuery.trim();
    if (!keyword || keyword.length > 80) {
      report('请输入 1–80 个字符的游戏名称。', 'mapping');
      return;
    }
    const epoch = mappingEpoch;
    const mapping = currentMapping;
    busyAction = 'search';
    mappingError = '';
    mainError = '';
    games = [];
    gameId = '';
    areaId = '';
    searched = false;
    try {
      const result = await dispatch({ action: 'search', payload: { keyword } });
      if (!mounted || epoch !== mappingEpoch || !mappingDialog?.open) return;
      games = result;
      searched = true;
      const chosen = result.find((game) => game.id === mapping?.gameId) ?? result[0];
      gameId = chosen ? String(chosen.id) : '';
      areaId = chosen?.areas.some((area) => area.areaId === mapping?.areaId) ? String(mapping?.areaId) : '';
      if (!result.length) report('未找到游戏，请换个名称搜索。', 'mapping');
    } catch (error: unknown) {
      if (mounted && epoch === mappingEpoch && mappingDialog?.open) report(error, 'mapping');
    } finally {
      if (mounted) busyAction = null;
    }
  }

  async function saveMapping(): Promise<void> {
    if (!mappingTarget) return;
    if (!currentGame || !Number.isInteger(currentGame.id) || currentGame.id <= 0) {
      report('请先搜索并选择一个 PC 游戏。', 'mapping');
      return;
    }
    if (areaId && !currentGame.areas.some((area) => String(area.areaId) === areaId)) {
      report('请选择列表中的有效区服，或使用自动区服。', 'mapping');
      return;
    }
    if (minutes === undefined || !Number.isFinite(minutes) || minutes < 1 || minutes > 1440
      || !Number.isInteger(minutes * 2)) {
      report('加速时长应为 1–1440 分钟，以 0.5 分钟为步长。', 'mapping');
      return;
    }
    const epoch = mappingEpoch;
    const saved = await mutate({ action: 'mapping', payload: {
      key: taskKey(mappingTarget.activity.id, mappingTarget.task.id),
      gameId: currentGame.id, areaId: areaId ? Number(areaId) : null, seconds: minutes * 60,
    } }, 'mapping');
    if (mounted && saved && epoch === mappingEpoch) mappingDialog?.close();
  }

  function showReconcile(): void {
    if (editingLocked || !run?.pending) return;
    reconcileError = '';
    reconcileChecked = false;
    reconcileKey = pendingKey;
    reconcileDialog?.showModal();
  }

  async function reconcile(): Promise<void> {
    if (!reconcileChecked) {
      report('请先在官方活动页核对任务状态和“我的奖励”，再勾选确认。', 'reconcile');
      return;
    }
    if (!run?.pending || reconcileChanged) {
      reconcileChecked = false;
      report('待核对记录已更新，请关闭窗口并查看最新状态。', 'reconcile');
      return;
    }
    if (await mutate({ action: 'reconcile', payload: { confirmed: true } }, 'reconcile')) {
      if (mounted) reconcileDialog?.close();
    }
  }

  async function utility(action: UtilityAction, url?: string, dialog?: DialogName): Promise<void> {
    if (!mounted || utilityBusy) return;
    utilityBusy = action;
    mainError = '';
    try {
      if (action === 'external') await openExternal(validateActivityUrl(url ?? ''));
      else if (action === 'directory') await openDataDir();
      else await requestExit();
    } catch (error: unknown) {
      report(error, dialog);
    } finally {
      if (mounted) utilityBusy = null;
    }
  }
</script>

<svelte:head>
  <title>AutoBiubiu · 活动助手</title>
  <meta name="description" content="本地 biubiu 活动助手，整理活动、合并加速任务并保存执行记录。" />
</svelte:head>

<div class="desktop-shell">
  <TitleBar onerror={(error) => report(error)} />
  <div class="desktop-body">
    <nav class="nav-rail" aria-label="主导航">
      <div class="rail-brand" title="AutoBiubiu"><Icon name="arrow" size={25} /></div>
      <button type="button" class:current={page === 'activities'} aria-current={page === 'activities' ? 'page' : undefined}
        title="活动" onclick={() => { page = 'activities'; }}><Icon name="activity" size={22} /><span>活动</span></button>
      <button type="button" class:current={page === 'rewards'} aria-current={page === 'rewards' ? 'page' : undefined}
        title="我的奖励" onclick={() => { page = 'rewards'; }}><Icon name="gift" size={22} /><span>奖励</span></button>
      <button type="button" class:current={page === 'logs'} aria-current={page === 'logs' ? 'page' : undefined}
        title="执行日志" onclick={() => { page = 'logs'; }}><Icon name="logs" size={22} /><span>日志</span></button>
      <div class="rail-spacer"></div>
      <button type="button" class:current={page === 'settings'} aria-current={page === 'settings' ? 'page' : undefined}
        title="设置" onclick={() => { page = 'settings'; }}><Icon name="settings" size={22} /><span>设置</span></button>
      <span class="rail-version">v{snapshot?.version || '0.3.0'}</span>
    </nav>

    <main class="workspace">
      <header class="page-header">
        <div><h1 id="page-title">{pageTitles[page]}</h1><p>{pageDescriptions[page]}</p></div>
        <div class="account-area">
          <div class="connection-controls">
            <span class="connection" role="status"><span class="status-dot" class:active={connected && !stateError}></span>{connectionText}</span>
            <button type="button" class="secondary" disabled={editingLocked}
              onclick={() => mutate({ action: 'connect', payload: {} })}><Icon name="link" size={15} />{busyAction === 'connect' ? '正在连接…' : '连接客户端'}</button>
          </div>
          <MembershipInfo membership={snapshot?.membership} connected={connected && !stateError}
            loggedIn={Boolean(snapshot?.connection.loggedIn)} locked={editingLocked} refreshing={busyAction === 'membership'}
            onrefresh={() => { void mutate({ action: 'membership', payload: {} }); }} />
        </div>
      </header>

      {#if stateError || mainError || backgroundError || run?.pending}
        <div class="notifications">
          {#if stateError}
            <div class="alert" role="alert"><Icon name="info" /><p>{stateError} · 每 2 秒重试，当前为最后一次读取的数据。</p>
              <button type="button" class="text-button" onclick={() => refreshState()}>立即重试</button></div>
          {/if}
          {#if mainError}
            <div class="alert" role="alert"><Icon name="info" /><p>{mainError}</p><button type="button" class="icon-button" aria-label="收起提示" onclick={() => { mainError = ''; }}><Icon name="close" size={15} /></button></div>
          {/if}
          {#if backgroundError}
            <div class="alert" role="alert"><Icon name="info" /><p>后台需要处理：{backgroundError}</p></div>
          {/if}
          {#if run?.pending}
            <section class="pending" aria-labelledby="pending-title">
              <Icon name="info" /><div><h2 id="pending-title">有一笔操作结果待核对</h2><p>请在官方活动页查看任务和“我的奖励”，核对后继续。</p></div>
              <button type="button" class="secondary" disabled={editingLocked} onclick={showReconcile}>已核对，重新读取</button>
            </section>
          {/if}
        </div>
      {/if}

      {#if page === 'activities'}
        <div class="command-bar" aria-label="活动操作">
          <div class="command-actions">
            <button type="button" class="secondary" disabled={editingLocked} onclick={showImport}><Icon name="plus" size={16} />导入链接</button>
            <button type="button" class="text-button" disabled={editingLocked} title="只刷新已保存活动的公开内容，不连接客户端"
              onclick={() => mutate({ action: 'scan', payload: { publicOnly: true } })}><Icon name="refresh" size={15} />更新公开内容</button>
            <span class="command-divider"></span>
            <button type="button" class="text-button" disabled={editingLocked || !connected}
              title={connected ? '读取活动和当前账号进度' : '请先点击连接客户端'}
              onclick={() => mutate({ action: 'scan', payload: {} })}>刷新活动与进度</button>
          </div>
          <label class="activity-search"><Icon name="search" size={15} /><input type="search" bind:value={activityQuery} placeholder="搜索活动或游戏" aria-label="搜索活动或游戏" /></label>
        </div>

        <div class="workbench">
          <section class="activity-browser" aria-label="活动列表与详情">
            <div class="activity-list-pane">
              <div class="list-filter" aria-label="活动筛选">
                <button type="button" class:active={!onlySelected} aria-pressed={!onlySelected} onclick={() => { onlySelected = false; }}>全部 <span>{activities.length}</span></button>
                <button type="button" class:active={onlySelected} aria-pressed={onlySelected} onclick={() => { onlySelected = true; }}>已选 <span>{selectedIds.length}</span></button>
              </div>
              <div class="list-heading"><label class="select-all"><input type="checkbox" checked={allSelected} indeterminate={partlySelected}
                disabled={editingLocked || !visibleActivities.length} onchange={(event) => selectVisible(event.currentTarget.checked)} />全选本列表</label><span>{visibleActivities.length} 项活动</span></div>
              <!-- svelte-ignore a11y_no_noninteractive_tabindex (独立滚动区需要支持键盘聚焦和方向键滚动) -->
              <div class="activity-list" tabindex="0" role="region" aria-label="可选择的活动">
                {#each visibleActivities as activity (activity.id)}
                  <div class="activity-row" class:focused={focused?.id === activity.id}>
                    <label class="row-check"><input type="checkbox" checked={selected.has(activity.id)} disabled={editingLocked}
                      onchange={(event) => selectActivity(activity.id, event.currentTarget.checked)} /><span class="sr-only">选择 {activity.name}</span></label>
                    <button type="button" class="activity-item" aria-pressed={focused?.id === activity.id} onclick={() => { focusedId = activity.id; }}>
                      <span class="activity-name">{activity.name}</span>
                      <span class="activity-summary">{activity.tasks.filter((task) => task.type === 7).length} 项加速任务<span>·</span>{activity.isOver ? '已结束' : activity.endTime ? `${formatTime(activity.endTime, 'date')} 截止` : '时间待更新'}</span>
                    </button>
                    <span class="row-chevron"><Icon name="chevron" size={13} /></span>
                  </div>
                {:else}
                  <div class="list-empty"><Icon name="search" size={22} /><p>{!snapshot ? '正在读取…' : activityQuery ? '没有匹配的活动' : onlySelected ? '尚未选择活动' : '还没有活动'}</p>
                    {#if activityQuery || onlySelected}<button type="button" class="text-button" onclick={() => { activityQuery = ''; onlySelected = false; }}>显示全部</button>{/if}</div>
                {/each}
              </div>
              <div class="list-footer"><Icon name={accountVerified ? 'check' : 'info'} size={13} />{accountVerified ? '本次已核对账号进度' : '本机缓存 · 进度未核对'}</div>
            </div>
            <!-- svelte-ignore a11y_no_noninteractive_tabindex (详情独立滚动，保留键盘访问) -->
            <div class="activity-detail" tabindex="0" role="region" aria-label="活动详情">
              {#if focused}
                {#key focused.id}
                  <ActivityCard activity={focused} selected={selected.has(focused.id)} mappings={snapshot?.mappings ?? {}}
                    locked={editingLocked} clientAvailable={connected} {accountVerified}
                    onselect={selectActivity} onmapping={showMapping}
                    onexternal={(url) => { void utility('external', url); }}
                    onopen={(id) => { void mutate({ action: 'open', payload: { id } }); }} />
                {/key}
              {:else}
                <div class="empty-state detail-empty"><span class="empty-icon"><Icon name="activity" size={34} /></span>
                  <h3>{activityQuery || onlySelected ? '这里会显示活动详情' : snapshot ? '从一个活动开始' : '正在读取本机活动'}</h3>
                  <p>{activityQuery || onlySelected ? '调整左侧筛选，选择一个活动查看任务。' : '导入官方活动链接，或连接客户端后刷新活动。'}</p>
                  {#if !activities.length}<button type="button" class="secondary" disabled={editingLocked} onclick={showImport}><Icon name="plus" size={16} />导入活动</button>{/if}
                  <span class="empty-footnote">只查看缓存，不会连接 biubiu 客户端</span>
                </div>
              {/if}
            </div>
          </section>

          <aside class="execution-pane" aria-label="任务运行面板">
            <section class="run-panel" aria-labelledby="run-title" aria-busy={runBusy}>
              <div class="section-heading"><h2>运行状态</h2><span class="run-state"><span class="status-dot" class:active={runBusy}></span>{run ? statusLabels[run.status] : '读取中'}</span></div>
              <div class="timer"><span>已核对的加速时间</span><strong>{duration(run?.elapsed ?? 0)}</strong></div>
              <h3 id="run-title">{runTitle}</h3><p class="run-detail">{runDetail}</p>
              <dl class="plan-summary" aria-label="所选任务估算"><div><dt>已选活动</dt><dd>{selectedIds.length}<small> 项</small></dd></div><div><dt>游戏任务组</dt><dd>{selectedGroups.length}<small> 组</small></dd></div><div><dt>预计用时</dt><dd>{estimatedMinutes}<small> 分钟</small></dd></div></dl>
              <p class="estimate-note">缓存估算，不含结算缓冲与网络等待</p>
              <button type="button" class="primary full start-button" disabled={editingLocked || !connected || Boolean(run?.pending) || Boolean(run?.ownedGameId) || !selectedIds.length}
                onclick={() => mutate({ action: 'start', payload: { ids: [...selectedIds] } })}><Icon name="play" size={17} />{busyAction === 'start' ? '正在启动…' : '开始所选活动'}</button>
              {#if run?.ownedGameId && !runBusy}
                <button type="button" class="secondary full" disabled={editingLocked}
                  onclick={() => mutate({ action: 'stop-owned', payload: {} })}>{busyAction === 'stop-owned' ? '正在停止…' : '停止遗留加速'}</button>
              {/if}
              <button type="button" class="danger full stop-button" disabled={!runBusy || run?.status === 'stopping' || stopBusy || utilityBusy === 'exit'}
                onclick={stopTasks}><Icon name="stop" size={14} />{stopBusy || run?.status === 'stopping' ? '正在停止…' : '停止任务'}</button>
              {#if !connected}<p class="run-hint">首次连接可能重启客户端并弹出 Windows 确认，请先结束手头的加速。</p>{/if}
            </section>
            <section class="live-log" aria-labelledby="live-log-title"><div class="section-heading"><h2 id="live-log-title">最近日志</h2><button type="button" class="text-button" onclick={() => { page = 'logs'; }}>查看全部<Icon name="chevron" size={12} /></button></div>
              <!-- svelte-ignore a11y_no_noninteractive_tabindex (日志独立滚动，保留键盘访问) -->
              <div class="live-log-content" tabindex="0" role="region" aria-label="最近执行日志"><RecordList kind="logs" logs={logs.slice(0, 20)} compact /></div>
            </section>
          </aside>
        </div>
      {:else if page === 'rewards' || page === 'logs'}
        <section class="records-page" aria-labelledby="page-title">
          <div class="records-toolbar"><span>{page === 'logs' ? `本轮共 ${logs.length} 条记录` : `本轮共 ${rewards.length} 条奖励记录`}<span class="muted"> · 最新记录在前</span></span>
            <button type="button" class="secondary" disabled={utilityBusy !== null} onclick={() => utility('directory')}><Icon name="folder" size={16} />打开记录目录</button></div>
          <!-- svelte-ignore a11y_no_noninteractive_tabindex (完整记录独立滚动，保留键盘访问) -->
          <div class="records-content" tabindex="0" role="region" aria-label={pageTitles[page]}><RecordList kind={page} {logs} {rewards} /></div>
        </section>
      {:else}
        <section class="settings-page" aria-labelledby="page-title">
          <ClientSettingsPanel settings={snapshot?.clientSettings} locked={editingLocked} saving={busyAction === 'client-settings'}
            lockReason={run?.pending || run?.ownedGameId ? '请先核对待确认记录并停止遗留加速，再修改客户端设置。' : ''}
            onsave={(settings) => mutate({ action: 'client-settings', payload: settings })} />
          <h2 class="settings-section-title">个性化</h2>
          <div class="setting-row"><span class="setting-icon"><Icon name="sun" size={21} /></span><div class="setting-copy"><h3>外观</h3><p>选择喜欢的颜色，设置会自动保存在本机。</p></div>
            <div class="theme-options" aria-label="界面主题">
              <button type="button" class:active={theme === 'light'} aria-pressed={theme === 'light'} onclick={() => setTheme('light')}><Icon name="sun" size={15} />浅色</button>
              <button type="button" class:active={theme === 'dark'} aria-pressed={theme === 'dark'} onclick={() => setTheme('dark')}><Icon name="moon" size={15} />深色</button>
              <button type="button" class:active={theme === 'system'} aria-pressed={theme === 'system'} onclick={() => setTheme('system')}><Icon name="monitor" size={15} />跟随系统</button>
            </div>
          </div>
          <h2 class="settings-section-title">应用与数据</h2>
          <div class="setting-row"><span class="setting-icon"><Icon name="folder" size={21} /></span><div class="setting-copy"><h3>本机记录目录</h3><p class="data-directory">{snapshot?.dataDir || '正在读取目录…'}</p></div><button type="button" class="secondary" disabled={utilityBusy !== null} onclick={() => utility('directory')}>打开目录</button></div>
          <div class="setting-row"><span class="setting-icon"><Icon name="monitor" size={21} /></span><div class="setting-copy"><h3>关闭窗口时收起到托盘</h3><p>任务保持运行；需要完全退出时，使用下方的“退出应用”。</p></div><span class="setting-value">已启用</span></div>
          <div class="setting-row"><span class="setting-icon"><Icon name="clock" size={21} /></span><div class="setting-copy"><h3>计时与任务规则</h3><p>同一游戏、同一区服合并计时，以活动服务器的完成状态为准。手机端、邀请和游戏内任务需手动完成。</p></div></div>
          <div class="setting-row"><span class="setting-icon"><Icon name="power" size={21} /></span><div class="setting-copy"><h3>退出应用</h3><p>由桌面后台确认退出、停止加速并保存记录。</p></div><button type="button" class="secondary" disabled={utilityBusy !== null} onclick={() => utility('exit')}>{utilityBusy === 'exit' ? '正在退出…' : '退出应用'}</button></div>
          <div class="about-app"><span class="app-symbol"><Icon name="arrow" size={20} /></span><strong>AutoBiubiu</strong><span>版本 {snapshot?.version || '0.3.0'} · 本地活动助手</span><p>让等待有个好结果。</p></div>
        </section>
      {/if}
    </main>
  </div>
  <footer class="statusbar"><span class="statusbar-message" role="status"><span class="status-dot" class:active={runBusy}></span>
    {#if snapshot?.operation || busyAction}正在{snapshot?.operation || ({ connect: '连接客户端', 'client-settings': '保存客户端设置', membership: '刷新会员信息', scan: '刷新活动', search: '搜索游戏', import: '导入活动', mapping: '保存设置', start: '启动所选活动', open: '打开活动', reconcile: '重新读取', 'stop-owned': '停止遗留加速' } as Record<string, string>)[busyAction ?? ''] || '处理请求'}…{:else}{runTitle}{/if}</span>
    {#if page !== 'activities' && runBusy}<button type="button" class="status-stop" disabled={stopBusy || run?.status === 'stopping' || utilityBusy === 'exit'} onclick={stopTasks}>停止任务</button>{/if}
    <span class="statusbar-update">{snapshot?.updatedAt ? `更新于 ${formatTime(snapshot.updatedAt)}` : '尚未更新活动'}<span class="statusbar-zone"> · Asia/Shanghai</span></span>
  </footer>
</div>

<dialog bind:this={importDialog} aria-labelledby="import-title" aria-describedby="import-help">
  <button type="button" class="dialog-close" aria-label="关闭导入活动窗口" onclick={() => importDialog?.close()}>×</button>
  <h2 id="import-title">导入官方活动链接</h2><p id="import-help" class="muted">读取公开规则并保存到本机，不连接客户端，也不读取账号进度。</p>
  <form novalidate onsubmit={(event) => { event.preventDefault(); void importActivity(); }}>
    <label class="field" for="import-url">活动链接</label>
    <input id="import-url" class="full" type="url" bind:value={importUrl} required
      placeholder="https://www.biubiu001.com/activity/activityTemplate?activityUuid=…"
      aria-invalid={Boolean(importError)} aria-describedby="import-help import-error" disabled={busyAction === 'import'} />
    <p id="import-error" class="error" role="alert">{importError}</p>
    <button type="submit" class="primary full" disabled={editingLocked}>{busyAction === 'import' ? '正在读取公开活动…' : '读取活动'}</button>
  </form>
</dialog>

<dialog bind:this={mappingDialog} aria-labelledby="mapping-title" aria-describedby="mapping-task"
  onclose={() => { mappingEpoch += 1; }}>
  <button type="button" class="dialog-close" aria-label="关闭游戏设置窗口" onclick={() => mappingDialog?.close()}>×</button>
  <h2 id="mapping-title">选择游戏和区服</h2><p id="mapping-task" class="muted">{mappingTarget?.task.title}</p>
  {#if currentMapping}<p class="muted">已保存：{currentMapping.gameName} · {currentMapping.areaName || '上次 / 默认区服'}</p>{/if}
  <form novalidate onsubmit={(event) => { event.preventDefault(); void searchGames(); }}>
    <label class="field" for="game-query">游戏名称</label>
    <div class="search-row"><input id="game-query" bind:value={gameQuery} maxlength="80" placeholder="输入 PC 游戏名称" disabled={editingLocked} />
      <button type="submit" class="secondary" disabled={editingLocked || !connected}>{busyAction === 'search' ? '搜索中…' : '搜索'}</button></div>
  </form>
  <p class="muted">{!connected ? '请关闭此窗口并先点击“连接客户端”，再搜索游戏。' : searched ? `找到 ${games.length} 个游戏，请核对游戏和区服。` : '点击搜索后读取游戏及区服列表，打开此窗口不会自动连接。'}</p>
  <form novalidate onsubmit={(event) => { event.preventDefault(); void saveMapping(); }}>
    <label class="field" for="game-select">游戏</label>
    <select id="game-select" class="full" bind:value={gameId} disabled={editingLocked || !games.length}
      onchange={() => { areaId = ''; }}>
      <option value="" disabled>请先搜索并选择 PC 游戏</option>
      {#each games as game (game.id)}<option value={String(game.id)}>{game.name}{game.english ? ` · ${game.english}` : ''}</option>{/each}
    </select>
    <label class="field" for="area-select">区服</label>
    <select id="area-select" class="full" bind:value={areaId} disabled={editingLocked || !currentGame}>
      <option value="">使用上次区服 / 第一个可用区服</option>
      {#each currentGame?.areas ?? [] as area (area.areaId)}<option value={String(area.areaId)}>{area.name}</option>{/each}
    </select>
    <label class="field" for="minutes">需要加速的分钟数</label>
    <input id="minutes" class="full" type="number" min="1" max="1440" step="0.5" bind:value={minutes}
      disabled={editingLocked} aria-describedby="minutes-help mapping-error" />
    <p id="minutes-help" class="muted">1–1440 分钟，支持半分钟；另加 20 秒结算缓冲。</p>
    <p id="mapping-error" class="error" role="alert">{mappingError}</p>
    <button type="submit" class="primary full" disabled={editingLocked || !currentGame}>{busyAction === 'mapping' ? '正在保存…' : '保存设置'}</button>
  </form>
</dialog>

<dialog bind:this={reconcileDialog} aria-labelledby="reconcile-title" aria-describedby="reconcile-help">
  <button type="button" class="dialog-close" aria-label="关闭核对确认窗口" onclick={() => reconcileDialog?.close()}>×</button>
  <h2 id="reconcile-title">确认上次操作结果</h2>
  <p id="reconcile-help" class="muted">先在官方活动页核对任务状态和“我的奖励”。确认后解除待核对标记并重新读取本机状态；下次开始仍会核对服务器进度。</p>
  {#if run?.pending}
    <dl class="pending-details"><div><dt>操作</dt><dd>{run.pending.kind === 'claim' ? '领取机会' : run.pending.kind.startsWith('draw') ? '抽奖结果' : run.pending.kind}</dd></div>
      <div><dt>活动</dt><dd>{pendingActivity?.name || run.pending.activityName || run.pending.activityId || '请查看执行日志'}</dd></div>
      <div><dt>开始时间</dt><dd>{formatTime(run.pending.startedAt)}</dd></div></dl>
  {/if}
  {#if pendingActivity}
    <button type="button" class="secondary" disabled={utilityBusy !== null}
      onclick={() => utility('external', pendingActivity.url, 'reconcile')}>独立打开活动 ↗</button>
  {/if}
  <form novalidate onsubmit={(event) => { event.preventDefault(); void reconcile(); }}>
    <label class="confirmation"><input type="checkbox" bind:checked={reconcileChecked} disabled={editingLocked} /><span>我已核对任务状态与奖品，确认可以继续。</span></label>
    <p id="reconcile-error" class="error" role="alert">{reconcileError}</p>
    <button type="submit" class="primary full" disabled={editingLocked || !reconcileChecked || !run?.pending}>{busyAction === 'reconcile' ? '正在重新读取…' : '已核对，重新读取'}</button>
  </form>
</dialog>
