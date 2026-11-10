import { invoke, isTauri } from '@tauri-apps/api/core';
import type { ClientInstallation, DispatchRequest, MutationRequest, SearchGame, SearchRequest, Snapshot } from './types';

async function invokeDesktop<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  if (!isTauri()) throw new Error('当前窗口未接入桌面后台，请通过 Tauri 桌面应用启动。');
  return invoke<T>(command, args);
}

// 仅通过桌面 IPC 读取状态，不会主动连接客户端。
export function getState(): Promise<Snapshot> {
  return invokeDesktop<Snapshot>('get_state');
}

export function dispatch(request: SearchRequest): Promise<SearchGame[]>;
export function dispatch(request: MutationRequest): Promise<unknown>;
export function dispatch(request: DispatchRequest): Promise<unknown> {
  return invokeDesktop('dispatch', { action: request.action, payload: request.payload });
}

export function openExternal(url: string): Promise<void> {
  return invokeDesktop<void>('open_external', { url });
}

export function openDataDir(): Promise<void> {
  return invokeDesktop<void>('open_data_dir');
}

export function detectClientInstallations(): Promise<ClientInstallation[]> {
  return invokeDesktop<ClientInstallation[]>('detect_client_installations');
}

export function pickClientDirectory(): Promise<string | null> {
  return invokeDesktop<string | null>('pick_client_directory');
}

// 退出确认、停止加速和持久化记录均由桌面后台处理。
export function requestExit(): Promise<void> {
  return invokeDesktop<void>('request_exit');
}
