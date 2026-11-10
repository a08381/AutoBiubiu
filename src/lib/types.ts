export type TaskId = string | number;
export type Timestamp = string | number;

export interface Connection {
  connected: boolean;
  loggedIn: boolean;
}

export interface ActivityTask {
  id: TaskId;
  type: number;
  status?: number;
  title: string;
  description?: string;
  awardNum?: number;
  finishCount?: number;
  supported?: boolean;
  reason?: string;
  gameName?: string | null;
  seconds?: number | null;
}

export interface Activity {
  id: string;
  name: string;
  url: string;
  tasks: ActivityTask[];
  cover?: string;
  startTime?: Timestamp;
  endTime?: Timestamp;
  isOver?: boolean;
  tickets?: number;
  rules?: unknown;
  freeGiftModule?: unknown;
  error?: string;
}

export interface GameMapping {
  gameId: number;
  gameName: string;
  areaId?: number | null;
  areaName?: string;
  seconds?: number;
  manual?: boolean;
}

export interface GameArea {
  areaId: number;
  name: string;
}

export interface SearchGame {
  id: number;
  name: string;
  english: string;
  areas: GameArea[];
}

export interface PlannedTask {
  key: string;
  activityId: string;
  activityName: string;
  taskId: TaskId;
  title: string;
  seconds: number;
  status?: number;
  manual?: boolean;
  endTime?: Timestamp;
}

export interface TaskGroup {
  id: string;
  gameId: number;
  areaId?: number | null;
  gameName: string;
  tasks: PlannedTask[];
  seconds: number;
}

export interface Plan {
  groups: TaskGroup[];
  skipped: { key?: string; activityId?: string; reason: string }[];
  totalSeconds: number;
}

export interface RunLog {
  time: Timestamp;
  level: string;
  message: string;
}

export interface PrizeRecord {
  time: Timestamp;
  activityId?: string;
  activityName?: string;
  inventoryDetailId?: string | number | null;
  beforeTickets?: number;
  prizeName?: string;
  result?: unknown;
  detail?: unknown;
}

export interface PendingOperation {
  kind: string;
  activityId?: string;
  activityName?: string;
  taskId?: TaskId;
  startedAt?: Timestamp;
  beforeTickets?: number;
  inventoryDetailId?: string | number | null;
}

export type RunStatus = 'idle' | 'starting' | 'running' | 'stopping' | 'stopped'
  | 'completed' | 'error' | 'interrupted';

export interface RunState {
  status: RunStatus;
  logs: RunLog[];
  results: PrizeRecord[];
  pending: PendingOperation | null;
  group: TaskGroup | null;
  elapsed: number;
  message?: string;
  ownedGameId?: number | null;
  cleanupError?: string | null;
  day?: string;
  plan?: Plan;
  selectedIds?: string[];
  startedAt?: Timestamp;
}

export interface Membership {
  expiresAt: number | null;
  kind: 'full' | 'trial' | 'unknown';
  checkedAt: number | null;
}

export interface ClientSettings {
  executablePath: string;
  debugPort: number;
}

export interface ClientInstallation {
  executablePath: string;
  source: string;
}

export interface Snapshot {
  clientSettings?: ClientSettings;
  connection: Connection;
  membership?: Membership;
  operation: string | null;
  authenticated: boolean;
  updatedAt: Timestamp | null;
  activities: Activity[];
  mappings: Record<string, GameMapping>;
  plan: Plan;
  run: RunState;
  dataDir?: string;
  version?: string;
}

export type DispatchRequest =
  | { action: 'connect' | 'membership' | 'stop' | 'stop-owned'; payload: Record<string, never> }
  | { action: 'client-settings'; payload: ClientSettings }
  | { action: 'scan'; payload: { publicOnly?: boolean } }
  | { action: 'import'; payload: { url: string } }
  | { action: 'search'; payload: { keyword: string } }
  | { action: 'mapping'; payload: { key: string; gameId: number; areaId: number | null; seconds: number } }
  | { action: 'start'; payload: { ids: string[] } }
  | { action: 'open'; payload: { id: string } }
  | { action: 'reconcile'; payload: { confirmed: true } };

export type SearchRequest = Extract<DispatchRequest, { action: 'search' }>;
export type MutationRequest = Exclude<DispatchRequest, SearchRequest>;
