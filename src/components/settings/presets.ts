/**
 * Mail provider presets for the account form.
 *
 * Typing a full IMAP/SMTP matrix by hand is where most people give up, so the
 * form fills it from the address domain and only falls back to 自定义 when we
 * do not know the provider.
 */

import type { Protocol, TlsMode } from "../../lib/types";

/** One server endpoint of a preset. */
export interface ServerPreset {
  host: string;
  port: number;
  tls: TlsMode;
}

export interface ProviderPreset {
  id: string;
  name: string;
  /** Address domains that auto-select this preset while the user types. */
  domains: string[];
  imap: ServerPreset | null;
  /** Null when the provider has no POP3 service at all. */
  pop3: ServerPreset | null;
  smtp: ServerPreset | null;
  /** Shown under the password field — most providers refuse the login password. */
  hint: string | null;
}

/** The escape hatch; always last in the picker and never auto-selected. */
export const CUSTOM_PRESET = "custom";

export const PRESETS: ProviderPreset[] = [
  {
    id: "gmail",
    name: "Gmail",
    domains: ["gmail.com", "googlemail.com"],
    imap: { host: "imap.gmail.com", port: 993, tls: "tls" },
    pop3: { host: "pop.gmail.com", port: 995, tls: "tls" },
    smtp: { host: "smtp.gmail.com", port: 465, tls: "tls" },
    hint: "Gmail 需要在 Google 账号安全设置中开启两步验证，并生成「应用专用密码」，不能使用登录密码。",
  },
  {
    id: "outlook",
    name: "Outlook / Hotmail",
    domains: ["outlook.com", "hotmail.com", "live.com", "msn.com"],
    imap: { host: "outlook.office365.com", port: 993, tls: "tls" },
    pop3: { host: "outlook.office365.com", port: 995, tls: "tls" },
    smtp: { host: "smtp-mail.outlook.com", port: 587, tls: "starttls" },
    hint: "若账号已开启两步验证，请在微软账号安全页面生成应用密码后填写。",
  },
  {
    id: "qq",
    name: "QQ 邮箱",
    domains: ["qq.com", "vip.qq.com", "foxmail.com"],
    imap: { host: "imap.qq.com", port: 993, tls: "tls" },
    pop3: { host: "pop.qq.com", port: 995, tls: "tls" },
    smtp: { host: "smtp.qq.com", port: 465, tls: "tls" },
    hint: "QQ 邮箱需在「设置 → 账户」中开启 IMAP/SMTP 服务，并使用生成的授权码，不是 QQ 密码。",
  },
  {
    id: "netease",
    name: "网易 163",
    domains: ["163.com", "126.com", "yeah.net"],
    imap: { host: "imap.163.com", port: 993, tls: "tls" },
    pop3: { host: "pop.163.com", port: 995, tls: "tls" },
    smtp: { host: "smtp.163.com", port: 465, tls: "tls" },
    hint: "163 邮箱需在「设置 → POP3/SMTP/IMAP」中开启服务，并使用客户端授权码，不是登录密码。",
  },
  {
    id: "icloud",
    name: "iCloud",
    domains: ["icloud.com", "me.com", "mac.com"],
    imap: { host: "imap.mail.me.com", port: 993, tls: "tls" },
    pop3: null,
    smtp: { host: "smtp.mail.me.com", port: 587, tls: "starttls" },
    hint: "iCloud 邮箱只支持 IMAP，密码需在 Apple ID 页面生成「App 专用密码」。",
  },
  {
    id: CUSTOM_PRESET,
    name: "自定义",
    domains: [],
    imap: null,
    pop3: null,
    smtp: null,
    hint: null,
  },
];

export function presetById(id: string): ProviderPreset | undefined {
  return PRESETS.find((p) => p.id === id);
}

/** Match "someone@Gmail.com " → the Gmail preset. Unknown domains → undefined. */
export function presetForEmail(email: string): ProviderPreset | undefined {
  const domain = email.trim().toLowerCase().split("@")[1];
  if (!domain) return undefined;
  return PRESETS.find((p) => p.domains.includes(domain));
}

/** The receiving endpoint a preset offers for the selected protocol. */
export function recvPreset(
  preset: ProviderPreset,
  protocol: Protocol,
): ServerPreset | null {
  return protocol === "imap" ? preset.imap : preset.pop3;
}
