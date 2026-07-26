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
  AssistantDelta,
  AssistantReply,
  CategoryCount,
  ChatTurn,
  Conversation,
  DeleteReport,
  EmailMessage,
  EmbeddingSettingsInput,
  EmbeddingSettingsPublic,
  IndexStatus,
  LabelCount,
  LabelInput,
  MailLabel,
  McpServerInput,
  McpServerPublic,
  McpServerStatus,
  MemoryEntry,
  MemoryEvent,
  MemoryInput,
  MessagePage,
  MessageQuery,
  NotifyChannel,
  OutgoingMail,
  PrivacySettings,
  RerankerSettingsInput,
  RerankerSettingsPublic,
  SearchHit,
  SyncStatus,
  TestResult,
  TrackerHit,
  TrackerStats,
} from "./types";

// -- shell ------------------------------------------------------------------

/** "windows" | "macos" | "linux" | "android" | "ios" */
export const hostPlatform = () => invoke<string>("host_platform");

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
/**
 * Delete messages. Resolves with what actually went — a server that refused
 * still has the mail, and the caller has to restore those rows.
 */
export const deleteMessages = (ids: string[], onServer: boolean) =>
  invoke<DeleteReport>("delete_messages", { ids, onServer });
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

// -- embedding index --------------------------------------------------------

export const getEmbeddingSettings = () =>
  invoke<EmbeddingSettingsPublic>("get_embedding_settings");
export const setEmbeddingSettings = (input: EmbeddingSettingsInput) =>
  invoke<EmbeddingSettingsPublic>("set_embedding_settings", { input });
export const testEmbedding = () => invoke<TestResult>("test_embedding");

export const getRerankerSettings = () =>
  invoke<RerankerSettingsPublic>("get_reranker_settings");
export const setRerankerSettings = (input: RerankerSettingsInput) =>
  invoke<RerankerSettingsPublic>("set_reranker_settings", { input });

// -- MCP servers (tools the assistant borrows) ------------------------------

export const getMcpServers = () => invoke<McpServerPublic[]>("get_mcp_servers");
export const saveMcpServer = (input: McpServerInput) =>
  invoke<McpServerPublic[]>("save_mcp_server", { input });
export const deleteMcpServer = (id: string) =>
  invoke<McpServerPublic[]>("delete_mcp_server", { id });
/**
 * Connect to every enabled server and report what each one offers. This is the
 * test button and the status list at once — there is nothing else to test about
 * an MCP server than whether it connects and what tools it has.
 */
export const mcpStatus = () => invoke<McpServerStatus[]>("mcp_status");
/** Drop every cached session and connect again from scratch. */
export const reconnectMcp = () => invoke<McpServerStatus[]>("reconnect_mcp");

// -- labels -----------------------------------------------------------------

export const listLabels = () => invoke<MailLabel[]>("list_labels");
export const labelCounts = () => invoke<LabelCount[]>("label_counts");
export const saveLabel = (input: LabelInput) => invoke<MailLabel[]>("save_label", { input });
export const deleteLabel = (id: string) => invoke<MailLabel[]>("delete_label", { id });

// -- privacy / trackers -----------------------------------------------------

export const getPrivacySettings = () => invoke<PrivacySettings>("get_privacy_settings");
export const setPrivacySettings = (input: PrivacySettings) =>
  invoke<PrivacySettings>("set_privacy_settings", { input });
/** What one message wanted to load from somebody else's server. */
export const messageTrackers = (id: string) =>
  invoke<TrackerHit[]>("message_trackers", { id });
/** The heatmap, the worst offenders, and the totals behind them. */
export const trackerStats = () => invoke<TrackerStats>("tracker_stats");

export const indexStatus = () => invoke<IndexStatus>("index_status");
/**
 * Start the backfill and return the progress at that moment; it keeps running
 * in the background, so follow it with `onIndexStatus`.
 */
export const indexPending = () => invoke<IndexStatus>("index_pending");
export const clearIndex = () => invoke<IndexStatus>("clear_index");

// -- retrieval --------------------------------------------------------------

/** `limit` omitted means "as many as the reranker settings allow". */
export const searchMail = (query: string, limit?: number) =>
  invoke<SearchHit[]>("search_mail", { query, limit: limit ?? null });

// -- memory -----------------------------------------------------------------

export const listMemories = () => invoke<MemoryEntry[]>("list_memories");
/** Memories that stopped being true. Never injected; history only. */
export const listMemoryHistory = () => invoke<MemoryEntry[]>("list_memory_history");
/** What happened to one memory, or to the memory as a whole when `id` is absent. */
export const memoryEvents = (id?: string) =>
  invoke<MemoryEvent[]>("memory_events", { id: id ?? null });
export const saveMemory = (input: MemoryInput) =>
  invoke<MemoryEntry>("save_memory", { input });
export const deleteMemory = (id: string) => invoke<void>("delete_memory", { id });

// -- assistant --------------------------------------------------------------

export const listConversations = (limit?: number) =>
  invoke<Conversation[]>("list_conversations", { limit: limit ?? null });
export const conversationTurns = (conversationId: string) =>
  invoke<ChatTurn[]>("conversation_turns", { conversationId });
export const deleteConversation = (id: string) =>
  invoke<void>("delete_conversation", { id });

/**
 * Ask one question. A null `conversationId` starts a new conversation — read
 * the id back off `reply.turn.conversationId` to keep asking into it.
 */
export const assistantAsk = (conversationId: string | null, text: string) =>
  invoke<AssistantReply>("assistant_ask", { conversationId, text });

/** Carry out a draft the user approved. Nothing else sends it. */
export const confirmPendingAction = (id: string) =>
  invoke<void>("confirm_pending_action", { id });

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

/** Progress of the embedding backfill, pushed after every batch. */
export function onIndexStatus(cb: (s: IndexStatus) => void): Promise<UnlistenFn> {
  return listen<IndexStatus>("mailer://index-status", (ev) => cb(ev.payload));
}

/**
 * Fragments of an assistant answer, in order, as the model writes them.
 *
 * Tagged with the conversation, because a panel that has been reset must not
 * append text belonging to the question it stopped waiting for. What arrives here
 * is provisional: a round that streams prose and then calls a tool has streamed
 * something that is not the answer, so `assistantAsk`'s reply replaces it.
 */
export function onAssistantDelta(
  cb: (d: AssistantDelta) => void,
): Promise<UnlistenFn> {
  return listen<AssistantDelta>("mailer://assistant-delta", (ev) => cb(ev.payload));
}
