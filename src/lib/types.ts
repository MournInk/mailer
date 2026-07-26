/**
 * TypeScript mirror of `crates/mailer-core/src/types.rs`.
 * Field names are camelCase on both sides — keep the two files in sync.
 */

export type Protocol = "imap" | "pop3";
export type TlsMode = "tls" | "starttls" | "none";
export type Category = "verification" | "spam" | "normal" | "important";
export type ChannelKind = "telegram" | "qqbot" | "webhook" | "bark";
export type SyncPhase = "idle" | "connecting" | "fetching" | "classifying" | "error";

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
  apiBase: string;
  hasApiKey: boolean;
  model: string;
  temperature: number;
  autoDeleteSpam: boolean;
  extraInstructions: string;
}

export interface AiSettingsInput {
  enabled: boolean;
  apiBase: string;
  /** empty/undefined keeps the stored key */
  apiKey?: string | null;
  model: string;
  temperature: number;
  autoDeleteSpam: boolean;
  extraInstructions: string;
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

/** UI labels for categories (zh). */
export const CATEGORY_LABEL: Record<Category, string> = {
  verification: "验证码",
  important: "重要",
  spam: "垃圾邮件",
  normal: "普通",
};
