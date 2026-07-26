/**
 * Editor for one notification channel. The visible fields are driven entirely
 * by `kind` — each kind writes exactly the keys `notify.rs` reads out of
 * `channel.config`, and nothing else.
 *
 * Values typed for one kind are kept in the draft when the user switches kinds,
 * so flipping back and forth while comparing options never loses input.
 */

import { useState } from "react";
import type { Category, ChannelKind, NotifyChannel } from "../../lib/types";
import { CATEGORY_LABEL } from "../../lib/types";
import { Icon } from "../Icon";
import { KIND_META, KINDS } from "./channelKinds";

/** Category order used everywhere in the UI, most actionable first. */
const CATEGORIES: Category[] = ["verification", "important", "normal", "spam"];

/** Absolute http(s) URLs only — the backend refuses anything else outright. */
const HTTP_URL = /^https?:\/\/\S+$/i;

interface HeaderRow {
  key: string;
  value: string;
}

interface Draft {
  id: string;
  name: string;
  kind: ChannelKind;
  enabled: boolean;
  notifyCategories: Category[];
  /** Flat kind-specific values, keyed exactly as the backend expects. */
  cfg: Record<string, string>;
  headers: HeaderRow[];
}

/** config values arrive as unknown JSON — coerce scalars, drop the rest. */
function cfgString(v: unknown): string {
  if (typeof v === "string") return v;
  if (typeof v === "number" || typeof v === "boolean") return String(v);
  return "";
}

function draftFrom(channel: NotifyChannel | null): Draft {
  if (!channel) {
    return {
      id: "",
      name: "",
      kind: "telegram",
      enabled: true,
      notifyCategories: ["important"],
      cfg: { targetKind: "private" },
      headers: [],
    };
  }
  const cfg: Record<string, string> = {};
  for (const [k, v] of Object.entries(channel.config)) {
    if (k !== "headers") cfg[k] = cfgString(v);
  }
  if (!cfg.targetKind) cfg.targetKind = "private";

  const raw = channel.config.headers;
  const headers: HeaderRow[] =
    raw && typeof raw === "object" && !Array.isArray(raw)
      ? Object.entries(raw as Record<string, unknown>).map(([key, value]) => ({
          key,
          value: cfgString(value),
        }))
      : [];

  return {
    id: channel.id,
    name: channel.name,
    kind: channel.kind,
    enabled: channel.enabled,
    notifyCategories: channel.notifyCategories.length
      ? channel.notifyCategories
      : ["important"],
    cfg,
    headers,
  };
}

/** Build the kind-specific config blob; optional fields are omitted entirely. */
function buildConfig(draft: Draft): Record<string, unknown> {
  const v = (key: string) => draft.cfg[key]?.trim() ?? "";
  const opt = (key: string, value: string) => (value ? { [key]: value } : {});

  switch (draft.kind) {
    case "telegram":
      return {
        botToken: v("botToken"),
        chatId: v("chatId"),
        ...opt("apiBase", v("apiBase")),
      };
    case "qqbot":
      return {
        apiBase: v("apiBase"),
        targetKind: v("targetKind") || "private",
        targetId: v("targetId"),
        ...opt("accessToken", v("accessToken")),
      };
    case "bark":
      return { deviceKey: v("deviceKey"), ...opt("server", v("server")) };
    case "webhook": {
      const headers: Record<string, string> = {};
      for (const row of draft.headers) {
        const key = row.key.trim();
        if (key) headers[key] = row.value.trim();
      }
      return {
        url: v("url"),
        ...(Object.keys(headers).length > 0 ? { headers } : {}),
        ...opt("bodyTemplate", draft.cfg.bodyTemplate ?? ""),
      };
    }
  }
}

/** First problem in Chinese, or null. Mirrors the checks in `notify.rs`. */
function validate(draft: Draft): string | null {
  const v = (key: string) => draft.cfg[key]?.trim() ?? "";
  if (draft.notifyCategories.length === 0) return "请至少选择一个通知类别。";

  switch (draft.kind) {
    case "telegram":
      if (!v("botToken")) return "请填写 Bot Token。";
      if (!v("chatId")) return "请填写 Chat ID。";
      if (v("apiBase") && !HTTP_URL.test(v("apiBase"))) {
        return "接口地址需以 http:// 或 https:// 开头。";
      }
      return null;
    case "qqbot":
      if (!HTTP_URL.test(v("apiBase"))) {
        return "请填写 OneBot HTTP 接口地址，需以 http:// 或 https:// 开头。";
      }
      if (!/^\d+$/.test(v("targetId"))) return "QQ 号 / 群号只能是数字。";
      return null;
    case "bark":
      if (!v("deviceKey")) return "请填写 Bark 设备 Key。";
      if (v("server") && !HTTP_URL.test(v("server"))) {
        return "服务器地址需以 http:// 或 https:// 开头。";
      }
      return null;
    case "webhook":
      if (!HTTP_URL.test(v("url"))) {
        return "请填写 Webhook 地址，需以 http:// 或 https:// 开头。";
      }
      return null;
  }
}

export function ChannelForm({
  channel,
  saving,
  onSubmit,
  onCancel,
}: {
  channel: NotifyChannel | null;
  saving: boolean;
  onSubmit: (channel: NotifyChannel) => void;
  onCancel: () => void;
}) {
  const [draft, setDraft] = useState<Draft>(() => draftFrom(channel));
  const [error, setError] = useState<string | null>(null);

  const isEdit = draft.id !== "";
  const meta = KIND_META[draft.kind];

  const patch = (p: Partial<Draft>) => setDraft((d) => ({ ...d, ...p }));
  const setCfg = (key: string, value: string) =>
    setDraft((d) => ({ ...d, cfg: { ...d.cfg, [key]: value } }));

  const toggleCategory = (c: Category) =>
    setDraft((d) => ({
      ...d,
      notifyCategories: d.notifyCategories.includes(c)
        ? d.notifyCategories.filter((x) => x !== c)
        : [...d.notifyCategories, c],
    }));

  const submit = () => {
    const problem = validate(draft);
    if (problem) {
      setError(problem);
      return;
    }
    setError(null);
    onSubmit({
      id: draft.id,
      name: draft.name.trim() || meta.label,
      kind: draft.kind,
      enabled: draft.enabled,
      notifyCategories: draft.notifyCategories,
      config: buildConfig(draft),
    });
  };

  return (
    <form
      className="card set-section set-form fade-up"
      onSubmit={(e) => {
        e.preventDefault();
        submit();
      }}
    >
      <header className="set-section-head">
        <div className="set-section-text">
          <h2 className="set-section-title">{isEdit ? "编辑渠道" : "添加通知渠道"}</h2>
          <p className="set-section-sub">
            命中所选类别的邮件会实时推送到这里，适合把重要邮件转到手机上。
          </p>
        </div>
        <button
          type="button"
          className="icon-btn"
          onClick={onCancel}
          disabled={saving}
          title="取消"
          aria-label="取消"
        >
          <Icon name="x" size={16} />
        </button>
      </header>

      <div className="set-section-body">
        <div className="field">
          <span className="field-label">渠道类型</span>
          <div className="set-chips">
            {KINDS.map((kind) => (
              <button
                key={kind}
                type="button"
                className={`set-chip${draft.kind === kind ? " active" : ""}`}
                aria-pressed={draft.kind === kind}
                disabled={saving}
                onClick={() => patch({ kind })}
              >
                <Icon name={KIND_META[kind].icon} size={14} />
                {KIND_META[kind].label}
              </button>
            ))}
          </div>
          <p className="set-note">
            <Icon name="link" size={14} />
            <span>{meta.hint}</span>
          </p>
        </div>

        <div className="field">
          <label className="field-label" htmlFor="ch-name">
            渠道名称
          </label>
          <input
            id="ch-name"
            className="input"
            value={draft.name}
            disabled={saving}
            placeholder={meta.label}
            onChange={(e) => patch({ name: e.target.value })}
          />
        </div>

        {/* -- categories ---------------------------------------------------- */}
        <div className="field">
          <span className="field-label">推送以下类别的邮件</span>
          <div className="set-cats">
            {CATEGORIES.map((c) => {
              const on = draft.notifyCategories.includes(c);
              return (
                <button
                  key={c}
                  type="button"
                  className={`set-cat cat-${c}${on ? " on" : ""}`}
                  aria-pressed={on}
                  disabled={saving}
                  onClick={() => toggleCategory(c)}
                >
                  <Icon name={on ? "check" : "plus"} size={13} />
                  {CATEGORY_LABEL[c]}
                </button>
              );
            })}
          </div>
          <p className="field-hint">
            默认只推送「重要」。验证码类邮件同时会在应用内弹窗提醒。
          </p>
        </div>

        <div className="set-divider" />

        {/* -- kind-specific fields ------------------------------------------ */}
        {draft.kind === "telegram" && (
          <>
            <div className="set-grid">
              <div className="field">
                <label className="field-label" htmlFor="ch-token">
                  Bot Token
                </label>
                <input
                  id="ch-token"
                  className="input set-mono"
                  value={draft.cfg.botToken ?? ""}
                  disabled={saving}
                  autoComplete="off"
                  spellCheck={false}
                  placeholder="123456789:AA…"
                  onChange={(e) => setCfg("botToken", e.target.value)}
                />
              </div>
              <div className="field">
                <label className="field-label" htmlFor="ch-chat">
                  Chat ID
                </label>
                <input
                  id="ch-chat"
                  className="input set-mono"
                  value={draft.cfg.chatId ?? ""}
                  disabled={saving}
                  autoComplete="off"
                  spellCheck={false}
                  placeholder="123456789 或 @channel"
                  onChange={(e) => setCfg("chatId", e.target.value)}
                />
              </div>
            </div>
            <div className="field">
              <label className="field-label" htmlFor="ch-tg-base">
                接口地址（可选）
              </label>
              <input
                id="ch-tg-base"
                className="input set-mono"
                value={draft.cfg.apiBase ?? ""}
                disabled={saving}
                autoComplete="off"
                spellCheck={false}
                placeholder="https://api.telegram.org"
                onChange={(e) => setCfg("apiBase", e.target.value)}
              />
              <p className="field-hint">留空使用官方地址；网络受限时可填写自建反代。</p>
            </div>
          </>
        )}

        {draft.kind === "qqbot" && (
          <>
            <div className="field">
              <label className="field-label" htmlFor="ch-qq-base">
                HTTP 接口地址
              </label>
              <input
                id="ch-qq-base"
                className="input set-mono"
                value={draft.cfg.apiBase ?? ""}
                disabled={saving}
                autoComplete="off"
                spellCheck={false}
                placeholder="http://127.0.0.1:5700"
                onChange={(e) => setCfg("apiBase", e.target.value)}
              />
            </div>
            <div className="set-grid">
              <div className="field">
                <label className="field-label" htmlFor="ch-qq-kind">
                  发送到
                </label>
                <select
                  id="ch-qq-kind"
                  className="select"
                  value={draft.cfg.targetKind ?? "private"}
                  disabled={saving}
                  onChange={(e) => setCfg("targetKind", e.target.value)}
                >
                  <option value="private">私聊</option>
                  <option value="group">群聊</option>
                </select>
              </div>
              <div className="field">
                <label className="field-label" htmlFor="ch-qq-id">
                  {draft.cfg.targetKind === "group" ? "群号" : "QQ 号"}
                </label>
                <input
                  id="ch-qq-id"
                  className="input set-mono"
                  value={draft.cfg.targetId ?? ""}
                  disabled={saving}
                  inputMode="numeric"
                  autoComplete="off"
                  placeholder="10001"
                  onChange={(e) => setCfg("targetId", e.target.value)}
                />
              </div>
            </div>
            <div className="field">
              <label className="field-label" htmlFor="ch-qq-token">
                Access Token（可选）
              </label>
              <input
                id="ch-qq-token"
                className="input set-mono"
                value={draft.cfg.accessToken ?? ""}
                disabled={saving}
                autoComplete="off"
                spellCheck={false}
                placeholder="与 OneBot 服务配置一致"
                onChange={(e) => setCfg("accessToken", e.target.value)}
              />
            </div>
          </>
        )}

        {draft.kind === "bark" && (
          <div className="set-grid">
            <div className="field">
              <label className="field-label" htmlFor="ch-bark-key">
                设备 Key
              </label>
              <input
                id="ch-bark-key"
                className="input set-mono"
                value={draft.cfg.deviceKey ?? ""}
                disabled={saving}
                autoComplete="off"
                spellCheck={false}
                placeholder="Bark App 首页复制"
                onChange={(e) => setCfg("deviceKey", e.target.value)}
              />
            </div>
            <div className="field">
              <label className="field-label" htmlFor="ch-bark-server">
                服务器地址（可选）
              </label>
              <input
                id="ch-bark-server"
                className="input set-mono"
                value={draft.cfg.server ?? ""}
                disabled={saving}
                autoComplete="off"
                spellCheck={false}
                placeholder="https://api.day.app"
                onChange={(e) => setCfg("server", e.target.value)}
              />
            </div>
          </div>
        )}

        {draft.kind === "webhook" && (
          <>
            <div className="field">
              <label className="field-label" htmlFor="ch-url">
                请求地址
              </label>
              <input
                id="ch-url"
                className="input set-mono"
                value={draft.cfg.url ?? ""}
                disabled={saving}
                autoComplete="off"
                spellCheck={false}
                placeholder="https://example.com/hook"
                onChange={(e) => setCfg("url", e.target.value)}
              />
            </div>

            <div className="field">
              <span className="field-label">自定义请求头（可选）</span>
              {draft.headers.map((row, i) => (
                <div key={i} className="set-hdr-row">
                  <input
                    className="input set-mono"
                    value={row.key}
                    disabled={saving}
                    autoComplete="off"
                    spellCheck={false}
                    placeholder="Authorization"
                    aria-label="请求头名称"
                    onChange={(e) =>
                      patch({
                        headers: draft.headers.map((h, j) =>
                          j === i ? { ...h, key: e.target.value } : h,
                        ),
                      })
                    }
                  />
                  <input
                    className="input set-mono"
                    value={row.value}
                    disabled={saving}
                    autoComplete="off"
                    spellCheck={false}
                    placeholder="Bearer …"
                    aria-label="请求头内容"
                    onChange={(e) =>
                      patch({
                        headers: draft.headers.map((h, j) =>
                          j === i ? { ...h, value: e.target.value } : h,
                        ),
                      })
                    }
                  />
                  <button
                    type="button"
                    className="icon-btn"
                    disabled={saving}
                    title="删除该请求头"
                    aria-label="删除该请求头"
                    onClick={() =>
                      patch({ headers: draft.headers.filter((_, j) => j !== i) })
                    }
                  >
                    <Icon name="x" size={15} />
                  </button>
                </div>
              ))}
              <button
                type="button"
                className="btn btn-sm set-add-hdr"
                disabled={saving}
                onClick={() =>
                  patch({ headers: [...draft.headers, { key: "", value: "" }] })
                }
              >
                <Icon name="plus" size={14} />
                添加请求头
              </button>
            </div>

            <div className="field">
              <label className="field-label" htmlFor="ch-body">
                请求体模板（可选）
              </label>
              <textarea
                id="ch-body"
                className="textarea set-mono"
                value={draft.cfg.bodyTemplate ?? ""}
                disabled={saving}
                spellCheck={false}
                placeholder={'{"text": "{{subject}} — {{summary}}"}'}
                onChange={(e) => setCfg("bodyTemplate", e.target.value)}
              />
              <p className="field-hint">
                留空则发送完整 JSON 事件。可用占位符：
                <code className="set-code">{"{{category}}"}</code>
                <code className="set-code">{"{{subject}}"}</code>
                <code className="set-code">{"{{from}}"}</code>
                <code className="set-code">{"{{summary}}"}</code>
                <code className="set-code">{"{{code}}"}</code>
                <code className="set-code">{"{{account}}"}</code>
                <code className="set-code">{"{{date}}"}</code>
                模板是合法 JSON 时按 JSON 发送，否则按纯文本发送。
              </p>
            </div>
          </>
        )}

        {error && (
          <p className="set-error" role="alert">
            <Icon name="alert" size={14} />
            <span>{error}</span>
          </p>
        )}
      </div>

      <footer className="set-form-foot">
        <span className="set-foot-hint">保存后可在列表中发送测试通知。</span>
        <button type="button" className="btn" onClick={onCancel} disabled={saving}>
          取消
        </button>
        <button type="submit" className="btn btn-primary" disabled={saving}>
          <Icon
            name={saving ? "loader" : "check"}
            size={15}
            className={saving ? "set-spin" : undefined}
          />
          {saving ? "保存中…" : "保存"}
        </button>
      </footer>
    </form>
  );
}
