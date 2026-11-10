<script lang="ts">
  export type IconName = 'activity' | 'gift' | 'logs' | 'settings' | 'refresh' | 'plus' | 'search'
    | 'play' | 'stop' | 'link' | 'folder' | 'power' | 'chevron' | 'clock' | 'check' | 'info'
    | 'sun' | 'moon' | 'monitor' | 'minimize' | 'maximize' | 'restore' | 'close' | 'arrow' | 'game';
  let { name, size = 18 }: { name: IconName; size?: number } = $props();
  const paths: Record<IconName, string> = {
    activity: 'M4 4h6v6H4z M14 4h6v6h-6z M4 14h6v6H4z M14 14h6v6h-6z',
    gift: 'M3 8h18v4H3z M5 12v9h14v-9 M12 8v13 M12 8H8a3 3 0 1 1 3-3l1 3Z M12 8h4a3 3 0 1 0-3-3l-1 3Z',
    logs: 'M5 3h14v18H5z M8 7h8 M8 11h8 M8 15h5',
    settings: 'M9 3h6l1 3 3 1 2 5-2 5-3 1-1 3H9l-1-3-3-1-2-5 2-5 3-1 1-3Z M15 12a3 3 0 1 1-6 0 3 3 0 0 1 6 0',
    refresh: 'M20 7v5h-5 M4 17v-5h5 M6.1 6.1A8 8 0 0 1 19.5 10 M4.5 14a8 8 0 0 0 13.4 3.9',
    plus: 'M12 5v14 M5 12h14',
    search: 'M16 16l5 5 M18 10a8 8 0 1 1-16 0 8 8 0 0 1 16 0',
    play: 'm8 4 12 8-12 8V4Z',
    stop: 'M6 6h12v12H6z',
    link: 'M10 13a5 5 0 0 0 7 0l3-3a5 5 0 0 0-7-7l-2 2 M14 11a5 5 0 0 0-7 0l-3 3a5 5 0 0 0 7 7l2-2',
    folder: 'M3 7V4h6l2 3h10v13H3V7Z',
    power: 'M12 2v10 M6 5a9 9 0 1 0 12 0',
    chevron: 'm9 5 7 7-7 7',
    clock: 'M12 7v5l3 2 M22 12a10 10 0 1 1-20 0 10 10 0 0 1 20 0',
    check: 'm5 12 4 4L19 6',
    info: 'M12 11v6 M12 7h.01 M22 12a10 10 0 1 1-20 0 10 10 0 0 1 20 0',
    sun: 'M12 1v2 M12 21v2 M1 12h2 M21 12h2 M4.2 4.2l1.4 1.4 M18.4 18.4l1.4 1.4 M4.2 19.8l1.4-1.4 M18.4 5.6l1.4-1.4 M17 12a5 5 0 1 1-10 0 5 5 0 0 1 10 0',
    moon: 'M20.8 13A9 9 0 0 1 11 3.2 9 9 0 1 0 20.8 13Z',
    monitor: 'M3 3h18v14H3z M12 17v4 M8 21h8',
    minimize: 'M5 12h14',
    maximize: 'M5 5h14v14H5z',
    restore: 'M8 8h11v11H8z M5 16V5h11',
    close: 'm6 6 12 12 M6 18 18 6',
    arrow: 'M5 19 19 5 M7 5h12v12',
    game: 'M7 7h10c3 0 4 4 4 9 0 3-3 3-5-1H8c-2 4-5 4-5 1 0-5 1-9 4-9Z M6 11h4 M8 9v4 M16 11h.01 M18 13h.01',
  };
</script>

<svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor"
  stroke-width="1.65" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
  <path d={paths[name]} />
</svg>
