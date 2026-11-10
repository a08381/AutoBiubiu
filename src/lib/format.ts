import type { ActivityTask, GameMapping, TaskId, Timestamp } from './types';

const dateTime = new Intl.DateTimeFormat('zh-CN', {
  timeZone: 'Asia/Shanghai', year: 'numeric', month: '2-digit', day: '2-digit',
  hour: '2-digit', minute: '2-digit', second: '2-digit', hour12: false,
});
const dateOnly = new Intl.DateTimeFormat('zh-CN', {
  timeZone: 'Asia/Shanghai', year: 'numeric', month: '2-digit', day: '2-digit',
});
const clock = new Intl.DateTimeFormat('zh-CN', {
  timeZone: 'Asia/Shanghai', hour: '2-digit', minute: '2-digit', second: '2-digit', hour12: false,
});

export function formatTime(value?: Timestamp | null, mode: 'full' | 'date' | 'clock' = 'full'): string {
  if (value === undefined || value === null || value === '') return '尚未记录';
  const date = new Date(value);
  if (!Number.isFinite(date.getTime())) return '时间待核对';
  return (mode === 'date' ? dateOnly : mode === 'clock' ? clock : dateTime).format(date);
}

export function formatMembershipTime(value?: number | null): string {
  if (value == null || !Number.isSafeInteger(value) || value < 946684800000 || value > 253402271999999) {
    return '客户端未提供到期时间';
  }
  const parts = Object.fromEntries(dateTime.formatToParts(new Date(value)).map((part) => [part.type, part.value]));
  return `${parts.year}年${parts.month}月${parts.day}日 ${parts.hour}:${parts.minute}:${parts.second}`;
}

export function isoTime(value: Timestamp): string | undefined {
  const date = new Date(value);
  return Number.isFinite(date.getTime()) ? date.toISOString() : undefined;
}

export function duration(seconds: number): string {
  const safe = Number.isFinite(seconds) ? Math.max(0, Math.floor(seconds)) : 0;
  return `${String(Math.floor(safe / 60)).padStart(2, '0')}:${String(safe % 60).padStart(2, '0')}`;
}

export function taskKey(activityId: string, taskId: TaskId): string {
  return `${activityId}:${taskId}`;
}

export function canConfigure(task: ActivityTask): boolean {
  return task.type === 7 && !/手机端|移动端|安卓端|iOS端|手游端/i.test(`${task.title} ${task.description ?? ''}`);
}

export function taskStatus(task: ActivityTask): string {
  if (task.status === 3) return '已领取';
  if (task.status === 2) return '可领取';
  return task.supported ? '待加速' : '其他条件';
}

export function mappingDescription(task: ActivityTask, mapping?: GameMapping): string {
  if (!canConfigure(task)) return task.reason || task.description || '此任务需手动完成';
  if (mapping) {
    const seconds = mapping.seconds ?? task.seconds;
    return `${mapping.gameName} · ${mapping.areaName || '上次 / 默认区服'} · ${seconds == null ? '时长待设置' : `${seconds / 60} 分钟`}`;
  }
  return task.reason || '待匹配游戏，请选择游戏、区服和时长';
}

export function errorMessage(error: unknown): string {
  if (error instanceof Error) return error.message;
  if (typeof error === 'string') return error;
  if (typeof error === 'object' && error !== null) {
    if ('message' in error && typeof error.message === 'string') return error.message;
    if ('error' in error && typeof error.error === 'string') return error.error;
  }
  return '操作失败，请查看执行日志后重试。';
}

export function prettyJson(value: unknown): string {
  return value === undefined ? '暂无返回详情' : JSON.stringify(value, null, 2) ?? '暂无返回详情';
}

export function validateActivityUrl(value: string): string {
  let url: URL;
  try { url = new URL(value.trim()); } catch {
    throw new Error('请粘贴完整的 biubiu 官方活动链接。');
  }
  if (url.origin !== 'https://www.biubiu001.com' || url.pathname !== '/activity/activityTemplate'
    || url.username || url.password) {
    throw new Error('活动地址应为 https://www.biubiu001.com/activity/activityTemplate。');
  }
  const id = url.searchParams.get('activityUuid');
  if (!id || !/^[a-zA-Z0-9_-]+$/.test(id)) throw new Error('链接缺少有效的 activityUuid 活动编号。');
  return url.href;
}
