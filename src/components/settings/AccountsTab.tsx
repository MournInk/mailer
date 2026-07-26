/**
 * Account management: the list of configured mailboxes plus the add/edit form.
 * Deleting an account drops every message it downloaded, so it always goes
 * through an inline confirmation instead of a single click.
 */

import { useState } from "react";
import * as api from "../../lib/api";
import { useApp } from "../../lib/store";
import type { AccountPublic, TlsMode } from "../../lib/types";
import { Icon } from "../Icon";
import { AccountForm } from "./AccountForm";

const TLS_LABEL: Record<TlsMode, string> = {
  tls: "SSL/TLS",
  starttls: "STARTTLS",
  none: "不加密",
};

/** 300 → "每 5 分钟". */
function intervalLabel(secs: number): string {
  if (secs >= 3600 && secs % 3600 === 0) return `每 ${secs / 3600} 小时`;
  if (secs >= 60 && secs % 60 === 0) return `每 ${secs / 60} 分钟`;
  return `每 ${secs} 秒`;
}

/** null = list only; { account: null } = add form; { account } = edit form. */
type Editing = { account: AccountPublic | null } | null;

export function AccountsTab() {
  const { accounts, refreshAccounts, refreshList, pushToast } = useApp();

  const [editing, setEditing] = useState<Editing>(null);
  const [confirmId, setConfirmId] = useState<string | null>(null);
  const [removing, setRemoving] = useState(false);

  const remove = async (account: AccountPublic) => {
    setRemoving(true);
    try {
      await api.deleteAccount(account.id);
      setConfirmId(null);
      await refreshAccounts();
      await refreshList();
      pushToast("ok", `已删除「${account.label}」`);
    } catch (e) {
      pushToast("error", `删除失败: ${e}`);
    } finally {
      setRemoving(false);
    }
  };

  // the form takes over the whole pane: editing one account while the others
  // scroll underneath is noise, not context
  if (editing) {
    return (
      <AccountForm
        /* remount when switching targets so the draft never leaks across */
        key={editing.account?.id ?? "new"}
        account={editing.account}
        onDone={() => setEditing(null)}
      />
    );
  }

  return (
    <>
      <div className="set-toolbar">
        <p className="set-toolbar-text">
          {accounts.length > 0
            ? `已添加 ${accounts.length} 个邮箱，邮件与密码只保存在本机。`
            : "邮箱密码只加密保存在本机，可随时删除。"}
        </p>
        <button
          className="btn btn-primary"
          onClick={() => {
            setConfirmId(null);
            setEditing({ account: null });
          }}
        >
          <Icon name="plus" size={16} />
          添加邮箱
        </button>
      </div>

      {accounts.length === 0 && (
        <div className="card set-empty">
          <span className="set-empty-mark">
            <Icon name="mail" size={18} />
          </span>
          <span className="set-empty-title">还没有邮箱账户</span>
          <p className="set-empty-body">
            添加第一个邮箱后，Mailer 会立即开始收信并交给 AI 分类。
          </p>
        </div>
      )}

      <div className="set-cards">
        {accounts.map((a) => (
          <article key={a.id} className="card set-card">
            <div className="set-card-main">
              <span
                className="set-avatar"
                style={{ background: `hsl(${a.colorHue} 55% 45%)` }}
              >
                {a.label.trim().charAt(0) || "?"}
              </span>
              <div className="set-card-text">
                <div className="set-card-title">{a.label}</div>
                <div className="set-card-mail">{a.email}</div>
                <div className="set-card-meta">
                  <span>
                    {a.protocol.toUpperCase()} · {a.host}:{a.port} · {TLS_LABEL[a.tls]}
                  </span>
                  <span>
                    {a.hasSmtp
                      ? `SMTP · ${a.smtpHost}:${a.smtpPort}`
                      : "未配置发件服务器"}
                  </span>
                  <span>{intervalLabel(a.syncIntervalSecs)}同步一次</span>
                </div>
              </div>
              <div className="set-card-actions">
                <button
                  className="btn btn-sm"
                  disabled={removing}
                  onClick={() => {
                    setConfirmId(null);
                    setEditing({ account: a });
                  }}
                >
                  <Icon name="edit" size={14} />
                  编辑
                </button>
                <button
                  className="btn btn-sm btn-danger"
                  disabled={removing}
                  onClick={() => setConfirmId(confirmId === a.id ? null : a.id)}
                >
                  <Icon name="trash" size={14} />
                  删除
                </button>
              </div>
            </div>

            {confirmId === a.id && (
              <div className="set-confirm" role="alertdialog">
                <Icon name="alert" size={15} />
                <span className="set-confirm-text">
                  删除「{a.label}」会同时清除本机已下载的全部邮件，此操作无法撤销。
                  服务器上的邮件不受影响。
                </span>
                <div className="set-confirm-actions">
                  <button
                    className="btn btn-sm"
                    disabled={removing}
                    onClick={() => setConfirmId(null)}
                  >
                    取消
                  </button>
                  <button
                    className="btn btn-sm btn-danger"
                    disabled={removing}
                    onClick={() => void remove(a)}
                  >
                    {removing ? "删除中…" : "确认删除"}
                  </button>
                </div>
              </div>
            )}
          </article>
        ))}
      </div>
    </>
  );
}
