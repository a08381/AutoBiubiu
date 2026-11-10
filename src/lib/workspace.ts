import type { Activity } from './types';

// 搜索与勾选相互独立，切换筛选不会丢失待执行活动。
export function filterActivities(activities: Activity[], query: string, selected: ReadonlySet<string>, onlySelected: boolean): Activity[] {
  const keyword = query.trim().toLocaleLowerCase();
  return activities.filter((activity) => (!onlySelected || selected.has(activity.id))
    && (!keyword || [activity.name, ...activity.tasks.flatMap((task) => [task.title, task.gameName ?? ''])]
      .some((text) => text.toLocaleLowerCase().includes(keyword))));
}

export function focusedActivity(activities: Activity[], id: string | null): Activity | undefined {
  return activities.find((activity) => activity.id === id) ?? activities[0];
}
