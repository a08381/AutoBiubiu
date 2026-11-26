(() => {
  const report = { rendered: false, ipc: false, importValidation: false, operationsBlocked: false,
    navigation: false, themeSwitch: false, activityFilter: false, activitySelection: false,
    activityDetails: false, windowControls: false, desktopLayout: false, membershipDisplay: false, clientSettings: false,
    updateBanner: false, shutdownBanner: false };
  const internals = window.__TAURI_INTERNALS__;
  const send = (extra = {}) => internals?.invoke('smoke_report', { report: { ...report, ...extra } }).catch((error) => console.error('smoke_report failed', error));
  const waitFor = async (check, timeout = 10000) => {
    const started = Date.now();
    while (Date.now() - started < timeout) {
      const value = check();
      if (value) return value;
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
    return false;
  };
  const click = (selector) => {
    const button = document.querySelector(selector);
    if (!button || button.disabled) throw new Error(`控件不可用：${selector}`);
    button.click();
  };
  const inputValue = (input, value) => {
    input.value = value;
    input.dispatchEvent(new Event('input', { bubbles: true }));
    input.dispatchEvent(new Event('change', { bubbles: true }));
  };
  const run = async () => {
    report.rendered = !!(await waitFor(() => document.querySelector('#page-title')));
    if (!report.rendered) return send({ error: '界面标记缺失' });
    let state;
    try {
      state = await internals?.invoke('get_state');
      report.ipc = !!state && state.run?.status === 'idle';
    } catch (error) {
      return send({ error: `读取状态失败：${error?.message || error}` });
    }

    const shutdownBanner = await waitFor(() => document.querySelector('[aria-label="自动关机提示"]'));
    const shutdownControls = !!shutdownBanner && [...shutdownBanner.querySelectorAll('button')]
      .some((button) => button.textContent === '取消关机');
    const previousShutdownAt = sessionStorage.getItem('smoke-shutdown-at');
    const shutdownPreserved = previousShutdownAt === null || Number(previousShutdownAt) === state.shutdownAt;
    sessionStorage.setItem('smoke-shutdown-at', String(state.shutdownAt));
    const initialSeconds = Number(/将在 (\d+) 秒后关机/.exec(shutdownBanner?.textContent)?.[1]);
    const countdown = initialSeconds > 0 && initialSeconds <= 30 && !!(await waitFor(() => {
      const seconds = Number(/将在 (\d+) 秒后关机/.exec(shutdownBanner.textContent)?.[1]);
      return seconds > 0 && seconds < initialSeconds;
    }, 3000));
    const banner = await waitFor(() => document.querySelector('[aria-label="应用更新提示"]'));
    const release = await internals.invoke('check_update');
    const bannerControls = !!banner && banner.textContent.includes(`v${release.latestVersion}`)
      && banner.textContent.includes(`v${release.currentVersion}`)
      && [...banner.querySelectorAll('button')].some((button) => button.textContent.includes('查看更新'));

    report.membershipDisplay = !!(await waitFor(() => {
      const panel = document.querySelector('.membership-info');
      if (state.connection.connected && state.connection.loggedIn && state.membership?.expiresAt) {
        const time = panel?.querySelector('time');
        return time?.getAttribute('datetime') === new Date(state.membership.expiresAt).toISOString()
          && /年\d{2}月\d{2}日 \d{2}:\d{2}:\d{2}/.test(time.textContent)
          && panel.textContent.includes('北京时间');
      }
      return panel?.textContent.includes('连接客户端后查看');
    }));

    const importButton = await waitFor(() => [...document.querySelectorAll('button')].find((button) => button.textContent?.includes('导入链接') && !button.disabled));
    if (!importButton) return send({ error: '导入按钮缺失' });
    importButton.click();
    const input = await waitFor(() => document.querySelector('dialog[aria-labelledby="import-title"] input'));
    if (!input) return send({ error: '导入弹窗缺失' });
    inputValue(input, 'https://example.invalid/not-an-activity');
    await new Promise((resolve) => setTimeout(resolve, 0));
    input.closest('form')?.dispatchEvent(new SubmitEvent('submit', { bubbles: true, cancelable: true }));
    report.importValidation = !!(await waitFor(() => {
      const dialog = input.closest('dialog');
      return !!dialog?.open && input.getAttribute('aria-invalid') === 'true'
        && !!dialog.querySelector('#import-error')?.textContent?.trim();
    }, 3000));
    input.closest('dialog')?.close();
    document.querySelector('[aria-label="收起提示"]')?.click();

    try { await internals?.invoke('dispatch', { action: 'connect', payload: {} }); }
    catch (_) { report.operationsBlocked = true; }
    if (!report.operationsBlocked) return send({ error: '冒烟模式仍允许业务操作' });

    // 隔离后台通过正常 IPC 返回合成活动，业务命令仍被阻止。
    const names = state.activities.map((activity) => activity.name);
    if (names.length !== 6) return send({ error: '隔离活动夹具缺失' });
    if (!await waitFor(() => document.querySelectorAll('.activity-row').length === names.length)) {
      return send({ error: '合成活动未渲染' });
    }

    const search = document.querySelector('[aria-label="搜索活动或游戏"]');
    inputValue(search, '冬日');
    report.activityFilter = !!(await waitFor(() => document.querySelectorAll('.activity-row').length === 1
      && document.querySelector('.activity-detail h3')?.textContent === names[1]));
    const rowCheck = document.querySelector('.row-check input');
    rowCheck.click();
    await new Promise((resolve) => setTimeout(resolve, 50));
    inputValue(search, '');
    await waitFor(() => document.querySelectorAll('.activity-row').length === names.length);
    click('.list-filter button:nth-child(2)');
    report.activitySelection = !!(await waitFor(() => document.querySelectorAll('.activity-row').length === names.length - 1));
    click('.list-filter button:first-child');
    await waitFor(() => document.querySelectorAll('.activity-row').length === names.length);
    document.querySelectorAll('.activity-item')[2].click();
    report.activityDetails = !!(await waitFor(() => document.querySelector('.activity-detail h3')?.textContent === names[2]));
    document.querySelectorAll('.row-check input')[1].click();
    document.querySelector('.activity-item')?.click();

    click('.nav-rail button[title="我的奖励"]');
    const rewardsPage = await waitFor(() => document.querySelector('#page-title')?.textContent === '我的奖励');
    const rewardsShutdown = document.querySelector('[aria-label="自动关机提示"]') === shutdownBanner;
    click('.nav-rail button[title="执行日志"]');
    const logsPage = await waitFor(() => document.querySelector('.records-content .log'));
    const logsShutdown = document.querySelector('[aria-label="自动关机提示"]') === shutdownBanner;
    click('.nav-rail button[title="设置"]');
    const settingsPage = await waitFor(() => document.querySelector('.theme-options'));
    const settingsShutdown = document.querySelector('[aria-label="自动关机提示"]') === shutdownBanner;
    const navigatedState = await internals.invoke('get_state');
    const shutdownNavigation = rewardsShutdown && logsShutdown && settingsShutdown && navigatedState.shutdownAt === state.shutdownAt;
    report.navigation = !!rewardsPage && !!logsPage && !!settingsPage;
    const pathInput = document.querySelector('#client-path');
    const portInput = document.querySelector('#client-port');
    const settingsForm = document.querySelector('.client-settings-panel');
    const initialSettings = pathInput?.value === state.clientSettings?.executablePath
      && Number(portInput?.value) === state.clientSettings?.debugPort;
    [...settingsForm.querySelectorAll('button')].find((button) => button.textContent.includes('重新识别')).click();
    const detected = await waitFor(() => pathInput.value.includes('client-fixture') && !portInput.disabled);
    const fixturePath = pathInput.value;
    inputValue(portInput, '0');
    const invalidPort = await waitFor(() => portInput.getAttribute('aria-invalid') === 'true' && settingsForm.querySelector('[type="submit"]').disabled);
    const newPort = state.clientSettings.debugPort === 9333 ? 9334 : 9333;
    inputValue(portInput, String(newPort));
    inputValue(pathInput, 'D:\\草稿目录\\biubiu.exe');
    await new Promise((resolve) => setTimeout(resolve, 2300));
    const draftPreserved = portInput.value === String(newPort) && pathInput.value === 'D:\\草稿目录\\biubiu.exe';
    inputValue(pathInput, fixturePath);
    await new Promise((resolve) => setTimeout(resolve, 0));
    click('.client-settings-panel button[type="submit"]');
    const saved = await waitFor(() => settingsForm.querySelector('[role="status"]')?.textContent.includes('设置已保存'));
    const savedState = await internals.invoke('get_state');
    const persisted = savedState.clientSettings.debugPort === newPort && savedState.clientSettings.executablePath === fixturePath;
    const settingsBounds = settingsForm.getBoundingClientRect();
    report.clientSettings = !!initialSettings && !!detected && !!invalidPort && draftPreserved && !!saved && persisted
      && settingsBounds.right <= innerWidth + 1 && settingsForm.scrollWidth <= settingsForm.clientWidth + 1;
    document.querySelector('[aria-label="收起提示"]')?.click();
    const savedTheme = localStorage.getItem('autobiubiu-theme');
    try {
      click('.theme-options button:nth-child(2)');
      const dark = await waitFor(() => document.documentElement.dataset.theme === 'dark');
      click('.theme-options button:first-child');
      const light = await waitFor(() => document.documentElement.dataset.theme === 'light');
      report.themeSwitch = !!dark && !!light;
    } finally {
      if (savedTheme === null) localStorage.removeItem('autobiubiu-theme');
      else localStorage.setItem('autobiubiu-theme', savedTheme);
    }
    click('.nav-rail button[title="活动"]');
    await waitFor(() => document.querySelector('.workbench'));
    click('.window-controls button[aria-label="最大化"]');
    const maximized = await waitFor(() => document.querySelector('.window-controls button[aria-label="还原窗口"]'));
    if (maximized) click('.window-controls button[aria-label="还原窗口"]');
    const restored = await waitFor(() => document.querySelector('.window-controls button[aria-label="最大化"]'));
    report.windowControls = !!maximized && !!restored;
    await new Promise((resolve) => setTimeout(resolve, 150));
    report.desktopLayout = document.documentElement.scrollWidth <= innerWidth
      && document.documentElement.scrollHeight <= innerHeight
      && ['.activity-browser', '.execution-pane', '.statusbar', '.account-area', '.membership-info'].every((selector) => {
        const bounds = document.querySelector(selector)?.getBoundingClientRect();
        return bounds && bounds.width > 0 && bounds.height > 0 && bounds.right <= innerWidth + 1 && bounds.bottom <= innerHeight + 1;
      }) && document.querySelector('.start-button')?.disabled === !state.connection.connected;
    // 双横幅占用高度时，最小窗口通过运行面板内滚动访问停止按钮。
    document.querySelector('.stop-button').scrollIntoView({ block: 'nearest' });
    const bannerBounds = banner?.getBoundingClientRect();
    const headerBounds = document.querySelector('.page-header')?.getBoundingClientRect();
    const commandsBounds = document.querySelector('.command-bar')?.getBoundingClientRect();
    const stopBounds = document.querySelector('.stop-button')?.getBoundingClientRect();
    const executionBounds = document.querySelector('.execution-pane')?.getBoundingClientRect();
    const bannerLayout = bannerBounds && headerBounds && commandsBounds && stopBounds && executionBounds
      && bannerBounds.width > 0 && bannerBounds.height > 0 && bannerBounds.right <= innerWidth + 1
      && banner.scrollWidth <= banner.clientWidth + 1
      && bannerBounds.top >= headerBounds.bottom - 1 && commandsBounds.top >= bannerBounds.bottom - 1
      && stopBounds.top >= executionBounds.top - 1 && stopBounds.bottom <= executionBounds.bottom + 1;
    document.querySelector('.execution-pane').scrollTop = 0;
    // 第一轮保留横幅供真实窗口截图；最小尺寸的第二轮验证关闭交互。
    let dismissed = true;
    if (innerWidth <= 900) {
      // 验证真实按钮 IPC 和错误呈现，隔离后台阻止打开系统浏览器。
      [...banner.querySelectorAll('button')].find((button) => button.textContent.includes('查看更新')).click();
      const openHandled = await waitFor(() => banner.querySelector('[role="alert"]')?.textContent.includes('冒烟测试模式不打开系统浏览器'));
      click('[aria-label="暂时关闭更新提示"]');
      dismissed = !!openHandled && !!(await waitFor(() => !document.querySelector('[aria-label="应用更新提示"]')));
    }
    report.updateBanner = !!bannerControls && !!bannerLayout && !!dismissed;
    const shutdownBounds = shutdownBanner?.getBoundingClientRect();
    const shutdownLayout = shutdownBounds && shutdownBounds.width > 0 && shutdownBounds.height > 0
      && shutdownBounds.right <= innerWidth + 1 && shutdownBanner.scrollWidth <= shutdownBanner.clientWidth + 1;
    report.shutdownBanner = !!shutdownControls && !!countdown && shutdownPreserved && shutdownNavigation && !!shutdownLayout;
    if (innerWidth <= 900) {
      click('[aria-label="自动关机提示"] button');
      const cancelFailed = await waitFor(() => shutdownBanner.querySelector('[role="alert"]')?.textContent.includes('模拟取消关机失败')
        && !shutdownBanner.querySelector('button').disabled);
      const failedState = await internals.invoke('get_state');
      click('[aria-label="自动关机提示"] button');
      const cancelled = await waitFor(() => !document.querySelector('[aria-label="自动关机提示"]'));
      const cancelledState = await internals.invoke('get_state');
      report.shutdownCancellation = !!cancelFailed && failedState.shutdownAt === state.shutdownAt && !!cancelled
        && cancelledState.shutdownAt === null
        && cancelledState.appSettings.shutdownAfterCompletion === state.appSettings.shutdownAfterCompletion;
    }
    const failed = Object.entries(report).filter(([, passed]) => !passed).map(([key]) => key);
    if (failed.length) return send({ error: `界面检查未通过：${failed.join('、')}` });
    send();
  };
  run().catch((error) => send({ error: `界面冒烟异常：${error?.message || error}` }));
})();
