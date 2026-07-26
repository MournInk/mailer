/**
 * TypeScript mirror of `crates/mailer-core/src/types.rs`.
 * Field names are camelCase on both sides — keep the two files in sync.
 */

export type Protocol = "imap" | "pop3";
export type TlsMode = "tls" | "starttls" | "none";
export type Category = "verification" | "spam" | "normal" | "important";
export type ChannelKind = "telegram" | "qqbot" | "webhook" | "bark";
export type SyncPhase = "idle" | "connecting" | "fetching" | "classifying" | "error";
/** Which wire protocol the configured endpoint speaks. */
export type AiProvider = "openai-compatible" | "openai-responses" | "anthropic" | "gemini";
export type RerankerKind = "none" | "rerank-api" | "llm-scoring";
export type MemoryKind = "preference" | "fact" | "contact";
export type ChatRole = "user" | "assistant" | "tool";

export interface AccountPublic {
  id: string;
  label: string;
  email: string;
  protocol: Protocol;
  host: string;
  port: number;
  username: string;
  tls: TlsMode;
  hasSmtp: boolean;
  smtpHost: string | null;
  smtpPort: number | null;
  smtpUsername: string | null;
  smtpTls: TlsMode | null;
  syncIntervalSecs: number;
  colorHue: number;
  createdAt: number;
}

/** Form payload for save_account / test_account. */
export interface AccountInput {
  id?: string | null;
  label: string;
  email: string;
  protocol: Protocol;
  host: string;
  port: number;
  username: string;
  /** empty/undefined keeps the stored password on update */
  password?: string | null;
  tls: TlsMode;
  smtp?: SmtpInput | null;
  syncIntervalSecs: number;
  colorHue: number;
}

export interface SmtpInput {
  host: string;
  port: number;
  username: string;
  password?: string | null;
  tls: TlsMode;
}

export interface AiAnalysis {
  category: Category;
  confidence: number;
  summary: string;
  verificationCode: string | null;
  deletable: boolean;
  reason: string;
}

export interface AttachmentMeta {
  filename: string;
  mime: string;
  size: number;
}

export interface EmailMessage {
  id: string;
  accountId: string;
  folder: string;
  uid: string;
  messageId: string | null;
  subject: string;
  fromName: string;
  fromAddr: string;
  toAddrs: string[];
  date: number;
  snippet: string;
  bodyText: string | null;
  bodyHtml: string | null;
  attachments: AttachmentMeta[];
  unread: boolean;
  starred: boolean;
  category: Category | null;
  analysis: AiAnalysis | null;
  receivedAt: number;
}

export interface MessageHeader {
  id: string;
  accountId: string;
  folder: string;
  subject: string;
  fromName: string;
  fromAddr: string;
  date: number;
  snippet: string;
  unread: boolean;
  starred: boolean;
  hasAttachments: boolean;
  category: Category | null;
  verificationCode: string | null;
  summary: string | null;
}

export interface MessageQuery {
  accountId?: string | null;
  folder?: string | null;
  category?: Category | null;
  unreadOnly?: boolean;
  starredOnly?: boolean;
  search?: string | null;
  limit?: number;
  offset?: number;
}

export interface MessagePage {
  items: MessageHeader[];
  total: number;
  unread: number;
}

export interface AiSettingsPublic {
  enabled: boolean;
  provider: AiProvider;
  apiBase: string;
  hasApiKey: boolean;
  model: string;
  temperature: number;
  autoDeleteSpam: boolean;
  extraInstructions: string;
}

export interface AiSettingsInput {
  enabled: boolean;
  provider: AiProvider;
  apiBase: string;
  /** empty/undefined keeps the stored key */
  apiKey?: string | null;
  model: string;
  temperature: number;
  autoDeleteSpam: boolean;
  extraInstructions: string;
}

export interface EmbeddingSettingsPublic {
  enabled: boolean;
  provider: AiProvider;
  apiBase: string;
  hasApiKey: boolean;
  model: string;
  /** requested vector width; 0 means "whatever the model returns" */
  dimensions: number;
}

export interface EmbeddingSettingsInput {
  enabled: boolean;
  provider: AiProvider;
  apiBase: string;
  /** empty/undefined keeps the stored key */
  apiKey?: string | null;
  model: string;
  dimensions: number;
}

export interface RerankerSettingsPublic {
  kind: RerankerKind;
  apiBase: string;
  hasApiKey: boolean;
  model: string;
  /** candidates fetched from the vector index before reranking */
  candidates: number;
  /** results kept after reranking */
  topN: number;
}

export interface RerankerSettingsInput {
  kind: RerankerKind;
  apiBase: string;
  /** empty/undefined keeps the stored key */
  apiKey?: string | null;
  model: string;
  candidates: number;
  topN: number;
}

export interface NotifyChannel {
  id: string;
  name: string;
  kind: ChannelKind;
  enabled: boolean;
  notifyCategories: Category[];
  /** kind-specific blob, see settings/ChannelsForm */
  config: Record<string, unknown>;
}

export interface SyncStatus {
  accountId: string;
  phase: SyncPhase;
  fetched: number;
  error: string | null;
  lastOkAt: number | null;
}

export interface AlertEvent {
  messageId: string;
  category: Category;
  accountEmail: string;
  from: string;
  subject: string;
  summary: string;
  verificationCode: string | null;
}

export interface TestResult {
  ok: boolean;
  message: string;
}

export interface CategoryCount {
  category: string; // Category | "pending"
  total: number;
  unread: number;
}

export interface OutgoingMail {
  accountId: string;
  to: string[];
  subject: string;
  body: string;
  inReplyTo?: string | null;
}

// -- retrieval ---------------------------------------------------------------

/** One message the retriever considers relevant to a question. */
export interface SearchHit {
  messageId: string;
  accountId: string;
  subject: string;
  fromName: string;
  fromAddr: string;
  date: number;
  excerpt: string;
  /** higher is better; comparable only within one result set */
  score: number;
}

export interface IndexStatus {
  indexed: number;
  total: number;
  model: string;
  /** true while a backfill is running */
  building: boolean;
  error: string | null;
}

// -- memory ------------------------------------------------------------------

export interface MemoryEntry {
  id: string;
  kind: MemoryKind;
  text: string;
  /** a message id, or "assistant" when inferred */
  source: string | null;
  createdAt: number;
  updatedAt: number;
}

/** Form payload for save_memory. */
export interface MemoryInput {
  /** undefined → create; set → update */
  id?: string | null;
  kind: MemoryKind;
  text: string;
  source?: string | null;
}

// -- assistant ---------------------------------------------------------------

export interface ToolCallRecord {
  name: string;
  /** arguments as JSON, for display and debugging */
  arguments: Record<string, unknown>;
  /** short human-readable outcome, never the full payload */
  summary: string;
  ok: boolean;
}

export interface ChatTurn {
  id: string;
  conversationId: string;
  role: ChatRole;
  content: string;
  toolCalls: ToolCallRecord[];
  citations: SearchHit[];
  createdAt: number;
}

export interface Conversation {
  id: string;
  title: string;
  createdAt: number;
  updatedAt: number;
}

/** An action the assistant proposes but will not perform unconfirmed. */
export interface PendingAction {
  id: string;
  /** currently only "send_mail" */
  kind: string;
  /** rendered for the user to read before approving */
  description: string;
  payload: Record<string, unknown>;
}

export interface AssistantReply {
  turn: ChatTurn;
  /** set when the assistant wants to send mail and is waiting on the user */
  pendingConfirmation: PendingAction | null;
}

/** UI labels for categories (zh). */
export const CATEGORY_LABEL: Record<Category, string> = {
  verification: "验证码",
  important: "重要",
  spam: "垃圾邮件",
  normal: "普通",
};

export const AI_PROVIDER_LABEL: Record<AiProvider, string> = {
  "openai-compatible": "OpenAI 兼容 · /chat/completions",
  "openai-responses": "OpenAI Responses · /responses",
  anthropic: "Anthropic · /v1/messages",
  gemini: "Gemini · generateContent",
};

export const RERANKER_KIND_LABEL: Record<RerankerKind, string> = {
  none: "不重排（按向量相似度）",
  "rerank-api": "重排接口（Jina / Cohere 等）",
  "llm-scoring": "让对话模型打分",
};

export const MEMORY_KIND_LABEL: Record<MemoryKind, string> = {
  preference: "偏好",
  fact: "事实",
  contact: "联系人",
};
