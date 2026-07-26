/**
 * Compose / reply modal. The store only holds the *initial* draft: everything
 * typed after that lives in local state, so a keystroke never re-renders the
 * mail list behind the modal. A failed send keeps the dialog (and the text)
 * exactly where it was.
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import * as api from "../lib/api";
import { useApp, type ComposeState } from "../lib/store";
import { Icon } from "./Icon";
import "./Compose.css";

/** Recipients are split on comma / semicolon (both widths) and newlines. */
const SEPARATORS = /[,;，；\n]+/;
/** Deliberately loose — the SMTP server is the real authority on addresses. */
const ADDRESS = /^[^\s@]+@[^\s@]+\.[^\s@]+$/;

/** "张三 <a@b.com>, c@d.com" → ["a@b.com", "c@d.com"] */
function parseRecipients(raw: string): string[] {
  return raw
    .split(SEPARATORS)
    .map((part) => {
      const angled = /<([^>]+)>/.exec(part);
      return (angled ? angled[1] : part).trim();
    })
    .filter((addr) => addr.length > 0);
}

export function Compose() {
  const { compose } = useApp();
  if (!compose) return null;
  return <ComposeDialog initial={compose} />;
}

function ComposeDialog({ initial }: { initial: ComposeState }) {
  const { accounts, closeCompose, pushToast } = useApp();

  const [accountId, setAccountId] = useState(initial.accountId);
  const [to, setTo] = useState(initial.to);
  const [subject, setSubject] = useState(initial.subject);
  const [body, setBody] = useState(initial.body);
  const [sending, setSending] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const toRef = useRef<HTMLInputElement | null>(null);
  const bodyRef = useRef<HTMLTextAreaElement | null>(null);
  const subjectRef = useRef<HTMLInputElement | null>(null);

  const account = useMemo(
    () => accounts.find((a) => a.id === accountId) ?? null,
    [accounts, accountId],
  );
  const canSend = !!account?.hasSmtp;
  const isReply = initial.inReplyTo !== null;

  // a reply already knows its recipient — start in the field the user needs
  useEffect(() => {
    const el = initial.to.trim() ? bodyRef.current : toRef.current;
    el?.focus();
  }, [initial.to]);

  // the body textarea grows with its content up to the CSS max-height
  useEffect(() => {
    const el = bodyRef.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${el.scrollHeight}px`;
  }, [body]);

  const send = useCallback(async () => {
    if (sending) return;

    const recipients = parseRecipients(to);
    if (recipients.length === 0) {
      setError("请至少填写一位收件人。");
      toRef.current?.focus();
      return;
    }
    const invalid = recipients.filter((addr) => !ADDRESS.test(addr));
    if (invalid.length > 0) {
      setError(`收件人地址格式有误：${invalid.join("、")}`);
      toRef.current?.focus();
      return;
    }
    if (!subject.trim() && !body.trim()) {
      setError("主题和正文都是空的，请至少填写一项再发送。");
      subjectRef.current?.focus();
      return;
    }
    if (!canSend) {
      setError("该账户未配置发件服务器，请先在设置中补充 SMTP 信息。");
      return;
    }

    setError(null);
    setSending(true);
    try {
      await api.sendMail({
        accountId,
        to: recipients,
        subject,
        body,
        inReplyTo: initial.inReplyTo,
      });
      closeCompose();
      pushToast("ok", "邮件已发送");
    } catch (e) {
      // keep the modal and the draft — the user should not have to retype
      setSending(false);
      setError(String(e));
      pushToast("error", `发送失败: ${e}`);
    }
  }, [
    sending,
    to,
    subject,
    body,
    canSend,
    accountId,
    initial.inReplyTo,
    closeCompose,
    pushToast,
  ]);

  // Esc closes, Cmd/Ctrl+Enter sends — both while focus is anywhere inside
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !sending) {
        e.preventDefault();
        closeCompose();
      } else if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
        e.preventDefault();
        void send();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [sending, closeCompose, send]);

  return (
    /* Backdrop clicks are ignored on purpose: a stray click must not discard a
       draft. Esc and 取消 are the two explicit ways out. */
    <div className="cp-backdrop" role="presentation">
      <div
        className="card cp-modal fade-up"
        role="dialog"
        aria-modal="true"
        aria-label={isReply ? "回复邮件" : "写邮件"}
      >
        <header className="cp-head">
          <h2 className="cp-title">{isReply ? "回复" : "写邮件"}</h2>
          <button
            className="icon-btn"
            onClick={closeCompose}
            disabled={sending}
            aria-label="关闭"
            title="关闭（Esc）"
          >
            <Icon name="x" size={16} />
          </button>
        </header>

        <div className="cp-body">
          <div className="field">
            <label className="field-label" htmlFor="cp-from">
              发件账户
            </label>
            <select
              id="cp-from"
              className="select"
              value={accountId}
              disabled={sending}
              onChange={(e) => setAccountId(e.target.value)}
            >
              {accounts.map((a) => (
                <option key={a.id} value={a.id} disabled={!a.hasSmtp}>
                  {a.label} · {a.email}
                  {a.hasSmtp ? "" : "（未配置发件服务器）"}
                </option>
              ))}
            </select>
            {!canSend && (
              <p className="field-hint cp-warn">
                该账户未配置发件服务器，请先在设置中补充 SMTP 信息。
              </p>
            )}
          </div>

          <div className="field">
            <label className="field-label" htmlFor="cp-to">
              收件人
            </label>
            <input
              id="cp-to"
              ref={toRef}
              className="input cp-mono"
              value={to}
              disabled={sending}
              placeholder="name@example.com，多个地址用逗号或分号分隔"
              onChange={(e) => setTo(e.target.value)}
            />
          </div>

          <div className="field">
            <label className="field-label" htmlFor="cp-subject">
              主题
            </label>
            <input
              id="cp-subject"
              ref={subjectRef}
              className="input"
              value={subject}
              disabled={sending}
              placeholder="邮件主题"
              onChange={(e) => setSubject(e.target.value)}
            />
          </div>

          <div className="field">
            <label className="field-label" htmlFor="cp-body">
              正文
            </label>
            <textarea
              id="cp-body"
              ref={bodyRef}
              className="textarea cp-textarea"
              value={body}
              disabled={sending}
              placeholder="写点什么…"
              onChange={(e) => setBody(e.target.value)}
            />
          </div>

          {error && (
            <p className="cp-error" role="alert">
              <Icon name="alert" size={14} />
              <span>{error}</span>
            </p>
          )}
        </div>

        <footer className="cp-foot">
          <span className="cp-shortcut">⌘/Ctrl + Enter 发送</span>
          <button className="btn" onClick={closeCompose} disabled={sending}>
            取消
          </button>
          <button
            className="btn btn-primary"
            onClick={() => void send()}
            disabled={sending || !canSend}
          >
            <Icon
              name={sending ? "loader" : "send"}
              size={15}
              className={sending ? "cp-spin" : undefined}
            />
            {sending ? "发送中…" : "发送"}
          </button>
        </footer>
      </div>
    </div>
  );
}
