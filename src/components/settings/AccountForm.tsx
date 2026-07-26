/**
 * Add / edit form for one mail account (`AccountInput`).
 *
 * Two conveniences carry the whole form: the provider preset picker fills the
 * IMAP/POP3 + SMTP matrix, and the address domain selects that preset by
 * itself. Anything the user types by hand wins — once a server field is edited
 * we stop auto-filling it.
 *
 * Secrets are write-only: the backend never sends a stored password back, so
 * on edit the password inputs start blank and an empty value means "keep".
 */

import { useState } from "react";
import * as api from "../../lib/api";
import { useApp } from "../../lib/store";
import type {
  AccountInput,
  AccountPublic,
  Protocol,
  TestResult,
  TlsMode,
} from "../../lib/types";
import { Icon } from "../Icon";
import { TestOutput } from "./parts";
import {
  CUSTOM_PRESET,
  PRESETS,
  presetById,
  presetForEmail,
  recvPreset,
  type ProviderPreset,
} from "./presets";

/** Deliberately loose — the mail server is the real authority on addresses. */
const ADDRESS = /^[^\s@]+@[^\s@]+\.[^\s@]+$/;

const PROTOCOL_LABEL: Record<Protocol, string> = {
  imap: "IMAP（推荐，保留服务器邮件）",
  pop3: "POP3",
};

const TLS_LABEL: Record<TlsMode, string> = {
  tls: "SSL/TLS",
  starttls: "STARTTLS",
  none: "不加密",
};

const SYNC_OPTIONS: Array<{ secs: number; label: string }> = [
  { secs: 60, label: "每 1 分钟" },
  { secs: 300, label: "每 5 分钟" },
  { secs: 900, label: "每 15 分钟" },
  { secs: 1800, label: "每 30 分钟" },
  { secs: 3600, label: "每小时" },
];

/** Avatar tints, evenly spread around the wheel. */
const HUES = [12, 40, 78, 140, 172, 205, 250, 288, 322];

/** Local draft: ports live as strings so the inputs can be emptied mid-edit. */
interface Draft {
  id: string | null;
  label: string;
  email: string;
  protocol: Protocol;
  host: string;
  port: string;
  username: string;
  password: string;
  tls: TlsMode;
  useSmtp: boolean;
  smtpHost: string;
  smtpPort: string;
  smtpUsername: string;
  smtpPassword: string;
  smtpTls: TlsMode;
  syncIntervalSecs: number;
  colorHue: number;
}

function draftFrom(account: AccountPublic | null): Draft {
  if (!account) {
    return {
      id: null,
      label: "",
      email: "",
      protocol: "imap",
      host: "",
      port: "993",
      username: "",
      password: "",
      tls: "tls",
      useSmtp: true,
      smtpHost: "",
      smtpPort: "465",
      smtpUsername: "",
      smtpPassword: "",
      smtpTls: "tls",
      syncIntervalSecs: 300,
      colorHue: HUES[Math.floor(Math.random() * HUES.length)],
    };
  }
  return {
    id: account.id,
    label: account.label,
    email: account.email,
    protocol: account.protocol,
    host: account.host,
    port: String(account.port),
    username: account.username,
    password: "",
    tls: account.tls,
    useSmtp: account.hasSmtp,
    smtpHost: account.smtpHost ?? "",
    smtpPort: account.smtpPort ? String(account.smtpPort) : "465",
    smtpUsername: account.smtpUsername ?? "",
    smtpPassword: "",
    smtpTls: account.smtpTls ?? "tls",
    syncIntervalSecs: account.syncIntervalSecs,
    colorHue: account.colorHue,
  };
}

export function AccountForm({
  account,
  onDone,
}: {
  account: AccountPublic | null;
  onDone: () => void;
}) {
  const { refreshAccounts, refreshList, pushToast } = useApp();

  const [draft, setDraft] = useState<Draft>(() => draftFrom(account));
  const [presetId, setPresetId] = useState<string>(
    () => presetForEmail(account?.email ?? "")?.id ?? CUSTOM_PRESET,
  );
  /** Set once a server field is typed into: stops preset auto-fill. */
  const [manualHost, setManualHost] = useState(false);
  /** Same idea for the username, which otherwise mirrors the address. */
  const [manualUser, setManualUser] = useState(() => account !== null);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<TestResult | null>(null);
  const [testing, setTesting] = useState(false);
  const [saving, setSaving] = useState(false);

  const isEdit = draft.id !== null;
  const preset = presetById(presetId);
  const busy = saving || testing;

  const patch = (p: Partial<Draft>) => setDraft((d) => ({ ...d, ...p }));

  /** Fill the server matrix from a preset. 自定义 only clears the selection. */
  const applyPreset = (id: string, protocol?: Protocol) => {
    setPresetId(id);
    setManualHost(false);
    const p = presetById(id);
    if (!p || p.id === CUSTOM_PRESET) {
      if (protocol) patch({ protocol });
      return;
    }
    setDraft((d) => {
      const proto = protocol ?? d.protocol;
      const recv = recvPreset(p, proto);
      return {
        ...d,
        protocol: proto,
        host: recv ? recv.host : d.host,
        port: recv ? String(recv.port) : d.port,
        tls: recv ? recv.tls : d.tls,
        smtpHost: p.smtp ? p.smtp.host : d.smtpHost,
        smtpPort: p.smtp ? String(p.smtp.port) : d.smtpPort,
        smtpTls: p.smtp ? p.smtp.tls : d.smtpTls,
      };
    });
  };

  const onEmail = (email: string) => {
    setDraft((d) => ({ ...d, email, username: manualUser ? d.username : email }));
    const match = presetForEmail(email);
    // Never fight the user: only auto-switch while the servers are untouched.
    if (match && match.id !== presetId && !manualHost) applyPreset(match.id);
  };

  const onProtocol = (protocol: Protocol) => {
    if (preset && preset.id !== CUSTOM_PRESET && !manualHost) {
      applyPreset(preset.id, protocol);
    } else {
      patch({ protocol });
    }
  };

  /** Any manual server edit; also drops the form out of "preset-driven" mode. */
  const onServerField = (p: Partial<Draft>) => {
    setManualHost(true);
    patch(p);
  };

  /** Returns the first problem in Chinese, or null when the draft is sendable. */
  const validate = (): string | null => {
    if (!ADDRESS.test(draft.email.trim())) return "请输入正确的邮箱地址。";
    if (!draft.host.trim()) return "请填写收件服务器地址。";
    const port = Number(draft.port);
    if (!Number.isInteger(port) || port < 1 || port > 65535) {
      return "收件服务器端口需为 1-65535 之间的整数。";
    }
    if (!draft.username.trim()) return "请填写登录用户名（通常就是邮箱地址）。";
    if (!isEdit && !draft.password) return "请填写密码或授权码。";
    if (draft.useSmtp) {
      if (!draft.smtpHost.trim()) return "请填写发件服务器地址，或关闭发件服务器。";
      const smtpPort = Number(draft.smtpPort);
      if (!Number.isInteger(smtpPort) || smtpPort < 1 || smtpPort > 65535) {
        return "发件服务器端口需为 1-65535 之间的整数。";
      }
    }
    return null;
  };

  const toInput = (): AccountInput => {
    const username = draft.username.trim() || draft.email.trim();
    return {
      id: draft.id,
      label: draft.label.trim() || draft.email.trim(),
      email: draft.email.trim(),
      protocol: draft.protocol,
      host: draft.host.trim(),
      port: Number(draft.port),
      username,
      // empty → the backend keeps the stored secret
      password: draft.password,
      tls: draft.tls,
      smtp: draft.useSmtp
        ? {
            host: draft.smtpHost.trim(),
            port: Number(draft.smtpPort),
            username: draft.smtpUsername.trim() || username,
            password: draft.smtpPassword,
            tls: draft.smtpTls,
          }
        : null,
      syncIntervalSecs: draft.syncIntervalSecs,
      colorHue: draft.colorHue,
    };
  };

  const test = async () => {
    const problem = validate();
    if (problem) {
      setError(problem);
      return;
    }
    setError(null);
    setResult(null);
    setTesting(true);
    try {
      setResult(await api.testAccount(toInput()));
    } catch (e) {
      setResult({ ok: false, message: String(e) });
    } finally {
      setTesting(false);
    }
  };

  const save = async () => {
    const problem = validate();
    if (problem) {
      setError(problem);
      return;
    }
    setError(null);
    setSaving(true);
    try {
      await api.saveAccount(toInput());
      await refreshAccounts();
      await refreshList();
      pushToast("ok", isEdit ? "账户已更新" : "账户已添加，正在收取邮件…");
      onDone();
    } catch (e) {
      // stay on the form — the user should not have to retype anything
      setSaving(false);
      setError(String(e));
    }
  };

  return (
    <form
      className="card set-section set-form fade-up"
      onSubmit={(e) => {
        e.preventDefault();
        void save();
      }}
    >
      <header className="set-section-head">
        <div className="set-section-text">
          <h2 className="set-section-title">{isEdit ? "编辑邮箱" : "添加邮箱"}</h2>
          <p className="set-section-sub">
            {isEdit
              ? "留空的密码字段表示保持原有密码不变。"
              : "选择服务商后会自动填好收发件服务器，只需补上账号与授权码。"}
          </p>
        </div>
        <button
          type="button"
          className="icon-btn"
          onClick={onDone}
          disabled={busy}
          title="取消"
          aria-label="取消"
        >
          <Icon name="x" size={16} />
        </button>
      </header>

      <div className="set-section-body">
        {/* -- provider ------------------------------------------------------ */}
        <div className="field">
          <span className="field-label">服务商</span>
          <div className="set-chips">
            {PRESETS.map((p) => (
              <PresetChip
                key={p.id}
                preset={p}
                active={presetId === p.id}
                onPick={() => applyPreset(p.id)}
              />
            ))}
          </div>
          {preset?.hint && (
            <p className="set-note">
              <Icon name="key" size={14} />
              <span>{preset.hint}</span>
            </p>
          )}
        </div>

        {/* -- identity ------------------------------------------------------ */}
        <div className="set-grid">
          <div className="field">
            <label className="field-label" htmlFor="ac-email">
              邮箱地址
            </label>
            <input
              id="ac-email"
              className="input set-mono"
              value={draft.email}
              disabled={busy}
              autoComplete="off"
              placeholder="name@example.com"
              onChange={(e) => onEmail(e.target.value)}
            />
          </div>
          <div className="field">
            <label className="field-label" htmlFor="ac-label">
              显示名称
            </label>
            <input
              id="ac-label"
              className="input"
              value={draft.label}
              disabled={busy}
              placeholder={draft.email || "工作邮箱"}
              onChange={(e) => patch({ label: e.target.value })}
            />
            <p className="field-hint">留空则使用邮箱地址。</p>
          </div>
        </div>

        <div className="set-grid">
          <div className="field">
            <label className="field-label" htmlFor="ac-interval">
              同步频率
            </label>
            <select
              id="ac-interval"
              className="select"
              value={draft.syncIntervalSecs}
              disabled={busy}
              onChange={(e) => patch({ syncIntervalSecs: Number(e.target.value) })}
            >
              {SYNC_OPTIONS.map((o) => (
                <option key={o.secs} value={o.secs}>
                  {o.label}
                </option>
              ))}
            </select>
          </div>
          <div className="field">
            <span className="field-label">标识色</span>
            <div className="set-hues">
              {HUES.map((hue) => (
                <button
                  key={hue}
                  type="button"
                  className={`set-hue${draft.colorHue === hue ? " active" : ""}`}
                  style={{ background: `hsl(${hue} 55% 45%)` }}
                  disabled={busy}
                  aria-label={`色相 ${hue}`}
                  aria-pressed={draft.colorHue === hue}
                  onClick={() => patch({ colorHue: hue })}
                />
              ))}
            </div>
          </div>
        </div>

        {/* -- receiving ----------------------------------------------------- */}
        <div className="set-divider" />
        <h3 className="set-sub-title">收件服务器</h3>

        <div className="set-grid">
          <div className="field">
            <label className="field-label" htmlFor="ac-protocol">
              协议
            </label>
            <select
              id="ac-protocol"
              className="select"
              value={draft.protocol}
              disabled={busy}
              onChange={(e) => onProtocol(e.target.value as Protocol)}
            >
              {(Object.keys(PROTOCOL_LABEL) as Protocol[]).map((p) => (
                <option key={p} value={p}>
                  {PROTOCOL_LABEL[p]}
                </option>
              ))}
            </select>
            {preset && draft.protocol === "pop3" && preset.pop3 === null &&
              preset.id !== CUSTOM_PRESET && (
                <p className="field-hint set-warn-text">
                  {preset.name} 不提供 POP3 服务，请改用 IMAP。
                </p>
              )}
          </div>
          <div className="field">
            <label className="field-label" htmlFor="ac-tls">
              加密方式
            </label>
            <select
              id="ac-tls"
              className="select"
              value={draft.tls}
              disabled={busy}
              onChange={(e) => onServerField({ tls: e.target.value as TlsMode })}
            >
              {(Object.keys(TLS_LABEL) as TlsMode[]).map((t) => (
                <option key={t} value={t}>
                  {TLS_LABEL[t]}
                </option>
              ))}
            </select>
          </div>
        </div>

        <div className="set-grid host-port">
          <div className="field">
            <label className="field-label" htmlFor="ac-host">
              服务器地址
            </label>
            <input
              id="ac-host"
              className="input set-mono"
              value={draft.host}
              disabled={busy}
              autoComplete="off"
              placeholder="imap.example.com"
              onChange={(e) => onServerField({ host: e.target.value })}
            />
          </div>
          <div className="field">
            <label className="field-label" htmlFor="ac-port">
              端口
            </label>
            <input
              id="ac-port"
              className="input set-mono"
              value={draft.port}
              disabled={busy}
              inputMode="numeric"
              placeholder="993"
              onChange={(e) => onServerField({ port: e.target.value })}
            />
          </div>
        </div>

        <div className="set-grid">
          <div className="field">
            <label className="field-label" htmlFor="ac-user">
              用户名
            </label>
            <input
              id="ac-user"
              className="input set-mono"
              value={draft.username}
              disabled={busy}
              autoComplete="off"
              placeholder={draft.email || "通常为完整邮箱地址"}
              onChange={(e) => {
                setManualUser(true);
                patch({ username: e.target.value });
              }}
            />
          </div>
          <div className="field">
            <label className="field-label" htmlFor="ac-pass">
              密码 / 授权码
            </label>
            <input
              id="ac-pass"
              className="input"
              type="password"
              value={draft.password}
              disabled={busy}
              autoComplete="new-password"
              placeholder={isEdit ? "保持不变" : "邮箱授权码"}
              onChange={(e) => patch({ password: e.target.value })}
            />
          </div>
        </div>

        {/* -- sending ------------------------------------------------------- */}
        <div className="set-divider" />
        <div className="set-sub-head">
          <h3 className="set-sub-title">发件服务器（SMTP）</h3>
          <button
            type="button"
            className={`switch${draft.useSmtp ? " on" : ""}`}
            role="switch"
            aria-checked={draft.useSmtp}
            aria-label="启用发件服务器"
            disabled={busy}
            onClick={() => patch({ useSmtp: !draft.useSmtp })}
          />
        </div>
        <p className="field-hint">不配置也能正常收信，只是无法在应用内写信与回复。</p>

        {draft.useSmtp && (
          <>
            <div className="set-grid host-port">
              <div className="field">
                <label className="field-label" htmlFor="ac-smtp-host">
                  服务器地址
                </label>
                <input
                  id="ac-smtp-host"
                  className="input set-mono"
                  value={draft.smtpHost}
                  disabled={busy}
                  autoComplete="off"
                  placeholder="smtp.example.com"
                  onChange={(e) => onServerField({ smtpHost: e.target.value })}
                />
              </div>
              <div className="field">
                <label className="field-label" htmlFor="ac-smtp-port">
                  端口
                </label>
                <input
                  id="ac-smtp-port"
                  className="input set-mono"
                  value={draft.smtpPort}
                  disabled={busy}
                  inputMode="numeric"
                  placeholder="465"
                  onChange={(e) => onServerField({ smtpPort: e.target.value })}
                />
              </div>
            </div>

            <div className="set-grid">
              <div className="field">
                <label className="field-label" htmlFor="ac-smtp-tls">
                  加密方式
                </label>
                <select
                  id="ac-smtp-tls"
                  className="select"
                  value={draft.smtpTls}
                  disabled={busy}
                  onChange={(e) => onServerField({ smtpTls: e.target.value as TlsMode })}
                >
                  {(Object.keys(TLS_LABEL) as TlsMode[]).map((t) => (
                    <option key={t} value={t}>
                      {TLS_LABEL[t]}
                    </option>
                  ))}
                </select>
              </div>
              <div className="field">
                <label className="field-label" htmlFor="ac-smtp-user">
                  用户名
                </label>
                <input
                  id="ac-smtp-user"
                  className="input set-mono"
                  value={draft.smtpUsername}
                  disabled={busy}
                  autoComplete="off"
                  placeholder="同收件用户名"
                  onChange={(e) => patch({ smtpUsername: e.target.value })}
                />
              </div>
            </div>

            <div className="field">
              <label className="field-label" htmlFor="ac-smtp-pass">
                密码 / 授权码
              </label>
              <input
                id="ac-smtp-pass"
                className="input"
                type="password"
                value={draft.smtpPassword}
                disabled={busy}
                autoComplete="new-password"
                placeholder={isEdit ? "保持不变" : "留空则与收件密码相同"}
                onChange={(e) => patch({ smtpPassword: e.target.value })}
              />
              <p className="field-hint">绝大多数邮箱的收发件密码相同，留空即可。</p>
            </div>
          </>
        )}

        {error && (
          <p className="set-error" role="alert">
            <Icon name="alert" size={14} />
            <span>{error}</span>
          </p>
        )}
        <TestOutput result={result} />
      </div>

      <footer className="set-form-foot">
        <button type="button" className="btn" onClick={onDone} disabled={busy}>
          取消
        </button>
        <button type="button" className="btn" onClick={() => void test()} disabled={busy}>
          <Icon
            name={testing ? "loader" : "shield"}
            size={15}
            className={testing ? "set-spin" : undefined}
          />
          {testing ? "测试中…" : "测试连接"}
        </button>
        <button type="submit" className="btn btn-primary" disabled={busy}>
          <Icon name={saving ? "loader" : "check"} size={15} className={saving ? "set-spin" : undefined} />
          {saving ? "保存中…" : "保存"}
        </button>
      </footer>
    </form>
  );
}

/** One provider chip; 自定义 carries no server data and just releases control. */
function PresetChip({
  preset,
  active,
  onPick,
}: {
  preset: ProviderPreset;
  active: boolean;
  onPick: () => void;
}) {
  return (
    <button
      type="button"
      className={`set-chip${active ? " active" : ""}`}
      aria-pressed={active}
      onClick={onPick}
    >
      {preset.name}
    </button>
  );
}
