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
  const [cc, setCc] = useState(initial.cc);
  const [bcc, setBcc] = useState(initial.bcc);
  // The copy fields stay folded away until there is something in them. Most
  // mail has none, and two empty inputs above the subject is two more things
  // to read past every time.
  const [showCopies, setShowCopies] = useState(
    () => initial.cc.trim() !== "" || initial.bcc.trim() !== "",
  );
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
  // A forward carries a quoted original but starts its own conversation, so it
  // has no parent. That is exactly what distinguishes it from a reply here.
  const isForward = !isReply && initial.body.trim() !== "";
  const manyRecipients = parseRecipients(initial.to).length + parseRecipients(initial.cc).length > 1;
  const kindLabel = isForward ? "转发" : isReply ? (manyRecipients ? "回复全部" : "回复") : "写邮件";
  const kindHint = isForward
    ? "把这封邮件转给别人，原文已经附在下面"
    : isReply
      ? manyRecipients
        ? "回复发件人以及这封邮件的其他收件人"
        : "回复这封邮件的发件人"
      : "从你的某个账户发出一封新邮件";

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
    const copies = parseRecipients(cc);
    const blind = parseRecipients(bcc);
    if (recipients.length === 0 && copies.length === 0 && blind.length === 0) {
      setError("请至少填写一位收件人。");
      toRef.current?.focus();
      return;
    }
    // Every field is checked, not just To: an unsendable address on the Cc
    // line fails the whole message at the relay, which is a worse place to
    // find out about it than here.
    const invalid = [...recipients, ...copies, ...blind].filter((a) => !ADDRESS.test(a));
    if (invalid.length > 0) {
      setError(`收件人地址格式有误：${invalid.join("、")}`);
      if (!showCopies && (copies.length > 0 || blind.length > 0)) setShowCopies(true);
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
        cc: copies,
        bcc: blind,
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
        aria-label={kindLabel}
      >
        <header className="cp-head">
          <span className="cp-mark" aria-hidden="true">
            <Icon name={isForward ? "forward" : isReply ? (manyRecipients ? "reply-all" : "reply") : "edit"} size={16} />
          </span>
          <span className="cp-headings">
            <h2 className="cp-title">{kindLabel}</h2>
            <span className="cp-sub">
              {kindHint}
            </span>
          </span>
          <button
            className="icon-btn cp-close"
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
              <p className="cp-warn">
                <Icon name="alert" size={14} />
                <span>该账户未配置发件服务器，暂时无法发送。请先到「设置 › 账户」中补充 SMTP 信息。</span>
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
            {!showCopies && (
              <button
                type="button"
                className="cp-copies-toggle"
                onClick={() => setShowCopies(true)}
              >
                添加抄送 / 密送
              </button>
            )}
          </div>

          {showCopies && (
            <>
              <div className="field">
                <label className="field-label" htmlFor="cp-cc">
                  抄送
                </label>
                <input
                  id="cp-cc"
                  className="input cp-mono"
                  value={cc}
                  disabled={sending}
                  placeholder="所有收件人都会看到这些地址"
                  onChange={(e) => setCc(e.target.value)}
                />
              </div>

              <div className="field">
                <label className="field-label" htmlFor="cp-bcc">
                  密送
                </label>
                <input
                  id="cp-bcc"
                  className="input cp-mono"
                  value={bcc}
                  disabled={sending}
                  placeholder="收到副本，但其他人看不到这些地址"
                  onChange={(e) => setBcc(e.target.value)}
                />
              </div>
            </>
          )}

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
          <span className="cp-shortcut">
            <kbd className="cp-kbd">⌘/Ctrl</kbd>
            <kbd className="cp-kbd">Enter</kbd>
            <span className="cp-shortcut-text">发送</span>
          </span>
          <button className="btn cp-cancel" onClick={closeCompose} disabled={sending}>
            取消
          </button>
          <button
            className="btn btn-primary cp-send"
            onClick={() => void send()}
            disabled={sending || !canSend}
            title={canSend ? "发送（⌘/Ctrl + Enter）" : "该账户未配置发件服务器"}
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
