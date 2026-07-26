/**
 * Typed wrappers around the Tauri IPC commands + event subscription helpers.
 * Every backend command is exposed here — components never call `invoke`
 * directly.
 */

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  AccountInput,
  AccountPublic,
  AiAnalysis,
  AiSettingsInput,
  AiSettingsPublic,
  AlertEvent,
  CategoryCount,
  EmailMessage,
  MessagePage,
  MessageQuery,
  NotifyChannel,
  OutgoingMail,
  SyncStatus,
  TestResult,
} from "./types";

// -- accounts ---------------------------------------------------------------

export const listAccounts = () => invoke<AccountPublic[]>("list_accounts");
export const saveAccount = (input: AccountInput) =>
  invoke<AccountPublic>("save_account", { input });
export const deleteAccount = (id: string) => invoke<void>("delete_account", { id });
export const testAccount = (input: AccountInput) =>
  invoke<TestResult>("test_account", { input });

// -- messages ---------------------------------------------------------------

export const listMessages = (query: MessageQuery) =>
  invoke<MessagePage>("list_messages", { query });
export const getMessage = (id: string) => invoke<EmailMessage>("get_message", { id });
export const markRead = (ids: string[], read: boolean) =>
  invoke<void>("mark_read", { ids, read });
export const setStarred = (id: string, starred: boolean) =>
  invoke<void>("set_starred", { id, starred });
export const deleteMessages = (ids: string[], onServer: boolean) =>
  invoke<void>("delete_messages", { ids, onServer });
export const syncNow = (accountId?: string | null) =>
  invoke<void>("sync_now", { accountId: accountId ?? null });
export const syncStatuses = () => invoke<SyncStatus[]>("sync_statuses");
export const categoryCounts = () => invoke<CategoryCount[]>("category_counts");

// -- AI ---------------------------------------------------------------------

export const getAiSettings = () => invoke<AiSettingsPublic>("get_ai_settings");
export const setAiSettings = (input: AiSettingsInput) =>
  invoke<AiSettingsPublic>("set_ai_settings", { input });
export const testAi = () => invoke<TestResult>("test_ai");
export const reclassify = (messageId: string) =>
  invoke<AiAnalysis>("reclassify", { messageId });

// -- channels ---------------------------------------------------------------

export const listChannels = () => invoke<NotifyChannel[]>("list_channels");
export const saveChannel = (channel: NotifyChannel) =>
  invoke<NotifyChannel>("save_channel", { channel });
export const deleteChannel = (id: string) => invoke<void>("delete_channel", { id });
export const testChannel = (id: string) => invoke<TestResult>("test_channel", { id });

// -- sending ----------------------------------------------------------------

export const sendMail = (mail: OutgoingMail) => invoke<void>("send_mail", { mail });

// -- events -----------------------------------------------------------------

export function onAlert(cb: (e: AlertEvent) => void): Promise<UnlistenFn> {
  return listen<AlertEvent>("mailer://alert", (ev) => cb(ev.payload));
}

export function onMailChanged(cb: (accountId: string) => void): Promise<UnlistenFn> {
  return listen<string>("mailer://mail-changed", (ev) => cb(ev.payload));
}

export function onSyncStatus(cb: (s: SyncStatus) => void): Promise<UnlistenFn> {
  return listen<SyncStatus>("mailer://sync-status", (ev) => cb(ev.payload));
}
