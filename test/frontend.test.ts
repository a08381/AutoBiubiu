import { beforeEach, describe, expect, it, vi } from 'vitest';

const desktop = vi.hoisted(() => ({ invoke: vi.fn(), isTauri: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => desktop);
import { detectClientInstallations, dispatch, getState, openDataDir, openExternal, pickClientDirectory, requestExit } from '../src/lib/ipc';
import {
  canConfigure,
  duration,
  errorMessage,
  formatMembershipTime,
  isoTime,
  mappingDescription,
  prettyJson,
  taskKey,
  taskStatus,
  validateActivityUrl,
} from '../src/lib/format';
import type { Activity, ActivityTask, GameMapping } from '../src/lib/types';
import { filterActivities, focusedActivity } from '../src/lib/workspace';
import { render } from 'svelte/server';
import MembershipInfo from '../src/lib/MembershipInfo.svelte';
import ClientSettingsPanel from '../src/lib/ClientSettingsPanel.svelte';

describe('客户端设置面板', () => {
  const props = { settings: { executablePath: 'C:\\Games\\biubiu\\biubiu.exe', debugPort: 9333 },
    locked: false, saving: false, onsave: async () => true };
  it('显示已保存路径、端口和自动识别入口', () => {
    const { body } = render(ClientSettingsPanel, { props });
    expect(body).toContain('C:\\Games\\biubiu\\biubiu.exe');
    expect(body).toContain('127.0.0.1:9333');
    expect(body).toContain('重新识别');
    expect(body).toContain('浏览目录');
    expect(body).toContain('保存客户端设置');
  });
  it('非法端口会显示错误且禁止保存', () => {
    const body = render(ClientSettingsPanel, { props: { ...props, settings: { ...props.settings, debugPort: 0 } } }).body;
    expect(body).toContain('aria-invalid="true"');
    expect(body).toContain('端口必须是 1–65535');
    expect(body).toMatch(/type="submit"[^>]*disabled/);
  });
  it('遗留加速或待核对记录时锁定设置并显示原因', () => {
    const body = render(ClientSettingsPanel, { props: { ...props, lockReason: '请先核对待确认记录并停止遗留加速' } }).body;
    expect(body).toContain('请先核对待确认记录并停止遗留加速');
    expect(body).toMatch(/id="client-path"[^>]*disabled/);
    expect(body).toMatch(/id="client-port"[^>]*disabled/);
  });
});

const membershipProps = {
  connected: true, loggedIn: true, locked: false, refreshing: false, onrefresh: () => {},
  membership: { expiresAt: Date.UTC(2099, 10, 12, 15, 59, 59), kind: 'full' as const, checkedAt: 1794268800000 },
};

describe('会员到期时间', () => {
  it('完整显示北京时间，保留具体日期和时分秒', () => {
    expect(formatMembershipTime(Date.UTC(2026, 10, 12, 15, 59, 59))).toBe('2026年11月12日 23:59:59');
    expect(formatMembershipTime(Date.UTC(2026, 10, 12, 16))).toBe('2026年11月13日 00:00:00');
    for (const value of [undefined, null, 0, -1, 1794499199, Number.NaN, Number.POSITIVE_INFINITY]) {
      expect(formatMembershipTime(value)).toBe('客户端未提供到期时间');
    }
  });

  it('渲染完整到期时间、时区和手动刷新入口', () => {
    const { body } = render(MembershipInfo, { props: membershipProps });
    expect(body).toContain('2099年11月12日 23:59:59');
    expect(body).toContain('北京时间');
    expect(body).toContain('datetime="2099-11-12T15:59:59.000Z"');
    expect(body).toContain('刷新会员到期时间');
  });

  it('断开连接或退出登录后隐藏上一个账号的到期时间', () => {
    const disconnected = render(MembershipInfo, { props: { ...membershipProps, connected: false } }).body;
    expect(disconnected).toContain('连接客户端后查看');
    expect(disconnected).not.toContain('2099年');
    const loggedOut = render(MembershipInfo, { props: { ...membershipProps, loggedIn: false } }).body;
    expect(loggedOut).toContain('请先登录 biubiu');
    expect(loggedOut).not.toContain('2099年');
  });

  it('明确区分未提供时间、试用会员和已经到期', () => {
    const missing = render(MembershipInfo, { props: { ...membershipProps, membership: undefined } }).body;
    expect(missing).toContain('客户端未提供到期时间');
    const trial = render(MembershipInfo, { props: { ...membershipProps,
      membership: { ...membershipProps.membership, kind: 'trial', expiresAt: Date.UTC(2001, 0, 1) } } }).body;
    expect(trial).toContain('试用会员到期');
    expect(trial).toContain('已到期');
    expect(trial).toContain('2001年01月01日 08:00:00');
  });
});

const activities: Activity[] = [
  { id: 'a', name: '周末加速福利', url: '', tasks: [task({ gameName: 'Counter-Strike 2' })] },
  { id: 'b', name: '限时抽奖活动', url: '', tasks: [task({ title: '完成每日签到' })] },
];

describe('桌面活动工作区', () => {
  it('按活动名、任务名和游戏名搜索，忽略空白与大小写', () => {
    expect(filterActivities(activities, '  counter-STRIKE ', new Set(), false)).toEqual([activities[0]]);
    expect(filterActivities(activities, '抽奖', new Set(), false)).toEqual([activities[1]]);
    expect(filterActivities(activities, '签到', new Set(), false)).toEqual([activities[1]]);
    expect(filterActivities(activities, '不存在', new Set(), false)).toEqual([]);
  });

  it('筛选已选活动不会修改已有勾选或原始活动数组', () => {
    const selected = new Set(['a', 'b']);
    expect(filterActivities(activities, '周末', selected, true)).toEqual([activities[0]]);
    expect([...selected]).toEqual(['a', 'b']);
    expect(filterActivities(activities, '', new Set(['b']), true)).toEqual([activities[1]]);
    expect(activities).toHaveLength(2);
  });

  it('刷新或筛选后聚焦项消失时回退首项，空列表没有详情', () => {
    expect(focusedActivity(activities, 'b')).toBe(activities[1]);
    expect(focusedActivity(activities, 'missing')).toBe(activities[0]);
    expect(focusedActivity(activities, null)).toBe(activities[0]);
    expect(focusedActivity([], 'a')).toBeUndefined();
  });
});

function task(overrides: Partial<ActivityTask> = {}): ActivityTask {
  return { id: 1, type: 7, title: '加速《游戏》10分钟', ...overrides };
}

describe('前端格式化与输入校验', () => {
  it('将时长限制为非负整数并保持分钟补零', () => {
    expect(duration(0)).toBe('00:00');
    expect(duration(125.9)).toBe('02:05');
    expect(duration(-10)).toBe('00:00');
    expect(duration(Number.NaN)).toBe('00:00');
    expect(duration(Number.POSITIVE_INFINITY)).toBe('00:00');
  });

  it('只接受官方活动模板链接并保留活动编号', () => {
    const url = 'https://www.biubiu001.com/activity/activityTemplate?activityUuid=Aa_01-b';
    expect(validateActivityUrl(url)).toBe(url);
    for (const invalid of [
      'https://example.org/activity/activityTemplate?activityUuid=x',
      'https://user@www.biubiu001.com/activity/activityTemplate?activityUuid=x',
      'http://www.biubiu001.com/activity/activityTemplate?activityUuid=x',
      'https://www.biubiu001.com/activity/other?activityUuid=x',
      'https://www.biubiu001.com/activity/activityTemplate?activityUuid=a%2Fb',
      'https://www.biubiu001.com/activity/activityTemplate?activityUuid=',
      'not a url',
    ]) {
      expect(() => validateActivityUrl(invalid)).toThrow();
    }
  });

  it('区分可配置的 PC 加速任务与移动端任务', () => {
    expect(canConfigure(task())).toBe(true);
    expect(canConfigure(task({ type: 11 }))).toBe(false);
    expect(canConfigure(task({ title: '加速游戏 iOS端 10分钟' }))).toBe(false);
    expect(canConfigure(task({ description: '移动端任务' }))).toBe(false);
  });

  it('优先显示已保存映射，并正确描述缺失映射原因', () => {
    const mapped: GameMapping = { gameId: 7, gameName: '测试游戏', areaName: '一区', seconds: 900 };
    expect(mappingDescription(task(), mapped)).toBe('测试游戏 · 一区 · 15 分钟');
    expect(mappingDescription(task({ reason: '活动规则不支持' }))).toBe('活动规则不支持');
    expect(mappingDescription(task({ supported: false }))).toBe('待匹配游戏，请选择游戏、区服和时长');
    expect(mappingDescription(task({ type: 11, description: '请分享活动' }))).toBe('请分享活动');
  });

  it('保持任务编号类型，映射状态和错误详情可读', () => {
    expect(taskKey('activity-1', '001')).toBe('activity-1:001');
    expect(taskStatus(task({ status: 3 }))).toBe('已领取');
    expect(taskStatus(task({ status: 2 }))).toBe('可领取');
    expect(taskStatus(task({ supported: true }))).toBe('待加速');
    expect(taskStatus(task({ supported: false }))).toBe('其他条件');
    expect(errorMessage(new Error('连接失败'))).toBe('连接失败');
    expect(errorMessage({ message: '输入无效' })).toBe('输入无效');
    expect(errorMessage({ error: '后台错误' })).toBe('后台错误');
    expect(errorMessage({ code: 500 })).toBe('操作失败，请查看执行日志后重试。');
  });

  it('为日志和奖励提供稳定的 JSON 与时间值', () => {
    expect(isoTime('2026-11-08T00:00:00.000Z')).toBe('2026-11-08T00:00:00.000Z');
    expect(isoTime('不是时间')).toBeUndefined();
    expect(prettyJson({ award: '测试', count: 1 })).toBe('{\n  "award": "测试",\n  "count": 1\n}');
    expect(prettyJson(undefined)).toBe('暂无返回详情');
  });
});

describe('桌面 IPC 契约', () => {
  beforeEach(() => {
    desktop.invoke.mockReset();
    desktop.isTauri.mockReset().mockReturnValue(true);
  });

  it('状态读取只调用 get_state，搜索保留 action 与 payload 结构', async () => {
    const snapshot = { version: '0.3.0', run: { status: 'idle' } };
    desktop.invoke.mockResolvedValueOnce(snapshot).mockResolvedValueOnce([]);
    expect(await getState()).toEqual(snapshot);
    expect(desktop.invoke).toHaveBeenNthCalledWith(1, 'get_state', undefined);
    expect(await dispatch({ action: 'search', payload: { keyword: '测试游戏' } })).toEqual([]);
    expect(desktop.invoke).toHaveBeenNthCalledWith(2, 'dispatch', { action: 'search', payload: { keyword: '测试游戏' } });
  });

  it('安装识别和目录选择只读，保存配置沿用串行操作通道', async () => {
    desktop.invoke.mockResolvedValue([]);
    await detectClientInstallations();
    await pickClientDirectory();
    const payload = { executablePath: 'C:\\biubiu\\biubiu.exe', debugPort: 9333 };
    await dispatch({ action: 'client-settings', payload });
    expect(desktop.invoke.mock.calls).toEqual([
      ['detect_client_installations', undefined], ['pick_client_directory', undefined],
      ['dispatch', { action: 'client-settings', payload }],
    ]);
  });

  it('会员刷新沿用现有受控操作通道', async () => {
    desktop.invoke.mockResolvedValue({ expiresAt: null });
    await dispatch({ action: 'membership', payload: {} });
    expect(desktop.invoke).toHaveBeenCalledWith('dispatch', { action: 'membership', payload: {} });
  });

  it('停止、外链、记录目录与退出使用固定的桌面命令', async () => {
    desktop.invoke.mockResolvedValue(undefined);
    await dispatch({ action: 'stop', payload: {} });
    await openExternal('https://www.biubiu001.com/activity/activityTemplate?activityUuid=test');
    await openDataDir();
    await requestExit();
    expect(desktop.invoke.mock.calls).toEqual([
      ['dispatch', { action: 'stop', payload: {} }],
      ['open_external', { url: 'https://www.biubiu001.com/activity/activityTemplate?activityUuid=test' }],
      ['open_data_dir', undefined],
      ['request_exit', undefined],
    ]);
  });

  it('普通浏览器不会尝试调用后台，后台错误原样返回供界面显示', async () => {
    desktop.isTauri.mockReturnValue(false);
    await expect(getState()).rejects.toThrow('当前窗口未接入桌面后台');
    expect(desktop.invoke).not.toHaveBeenCalled();
    desktop.isTauri.mockReturnValue(true);
    desktop.invoke.mockRejectedValueOnce(new Error('状态文件损坏'));
    await expect(getState()).rejects.toThrow('状态文件损坏');
  });
});
