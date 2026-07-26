/**
 * Global overlay layer, mounted on every view. It owns two things:
 *
 *  1. the alert queue — one modal at a time, the payoff of the AI pipeline:
 *     a verification code big enough to read across the desk, or the summary
 *     of an important mail;
 *  2. the toast stack — transient feedback that must never swallow a click
 *     meant for the app behind it.
 */

import { useCallback, useEffect, useRef } from "react";
import { Icon } from "./Icon";
import { useApp, type Toast } from "../lib/store";
import { CATEGORY_LABEL, type AlertEvent } from "../lib/types";
import "./AlertCenter.css";

/** Everything the focus trap is allowed to land on inside the modal. */
const FOCUSABLE =
  'button:not([disabled]), [href], input, select, textarea, [tabindex]:not([tabindex="-1"])';

const TOAST_ICON: Record<Toast["kind"], string> = {
  ok: "check",
  error: "alert",
  info: "mail",
};

export function AlertCenter() {
  const { alerts, toasts, dismissToast } = useApp();

  // Only the head of the queue is shown; dismissing it reveals the next one.
  const current = alerts[0];

  return (
    <>
      {current && (
        <AlertModal
          key={current.messageId}
          alert={current}
          queued={alerts.length - 1}
        />
      )}

      <div className="toast-stack" aria-live="polite">
        {toasts.map((t) => (
          <div key={t.id} className={`toast toast-${t.kind} fade-up`}>
            <span className="toast-icon">
              <Icon name={TOAST_ICON[t.kind]} size={16} />
            </span>
            <span className="toast-text">{t.text}</span>
            <button
              className="icon-btn toast-close"
              title="关闭"
              aria-label="关闭提示"
              onClick={() => dismissToast(t.id)}
            >
              <Icon name="x" size={14} />
            </button>
          </div>
        ))}
      </div>
    </>
  );
}

/** One alert as a modal. Escape / backdrop click dismiss it; focus is trapped
 *  inside while it is open and handed back to the caller on close. */
function AlertModal({ alert, queued }: { alert: AlertEvent; queued: number }) {
  const { dismissAlert, openAlertMessage, pushToast } = useApp();

  const cardRef = useRef<HTMLDivElement>(null);
  /** Auto-focused action, so Enter does the obvious thing straight away. */
  const primaryRef = useRef<HTMLButtonElement>(null);

  const code = alert.verificationCode;
  const isCode = alert.category === "verification" && !!code;
  const heroId = `alert-hero-${alert.messageId}`;

  const close = useCallback(
    () => dismissAlert(alert.messageId),
    [alert.messageId, dismissAlert],
  );

  const copy = useCallback(async () => {
    if (!code) return false;
    try {
      await navigator.clipboard.writeText(code);
      pushToast("ok", "验证码已复制");
      return true;
    } catch {
      pushToast("error", "复制失败，请手动选择验证码");
      return false;
    }
  }, [code, pushToast]);

  const copyAndClose = useCallback(async () => {
    // the code is the whole payload — keep the modal up if the copy failed
    if (await copy()) close();
  }, [copy, close]);

  // focus in on open, focus back out on close
  useEffect(() => {
    const previous = document.activeElement as HTMLElement | null;
    primaryRef.current?.focus();
    return () => previous?.focus();
  }, []);

  // Escape to dismiss + a Tab trap. Bound on the document (capture) so it also
  // holds when focus has drifted onto plain text inside the card.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        close();
        return;
      }
      if (e.key !== "Tab") return;
      const card = cardRef.current;
      if (!card) return;
      const nodes = Array.from(card.querySelectorAll<HTMLElement>(FOCUSABLE));
      if (nodes.length === 0) return;
      const first = nodes[0];
      const last = nodes[nodes.length - 1];
      const active = document.activeElement as HTMLElement | null;
      if (!active || !card.contains(active)) {
        e.preventDefault();
        (e.shiftKey ? last : first).focus();
      } else if (e.shiftKey && active === first) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && active === last) {
        e.preventDefault();
        first.focus();
      }
    };
    document.addEventListener("keydown", onKey, true);
    return () => document.removeEventListener("keydown", onKey, true);
  }, [close]);

  return (
    <div
      className="alert-backdrop"
      /* mousedown, not click: a selection that ends outside must not dismiss */
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) close();
      }}
    >
      <div
        ref={cardRef}
        className="alert-card fade-up"
        role="dialog"
        aria-modal="true"
        aria-labelledby={heroId}
      >
        <header className="alert-head">
          <span className={`badge badge-${alert.category}`}>
            <Icon name={isCode ? "key" : "alert"} size={12} />
            {CATEGORY_LABEL[alert.category]}
          </span>
          <button className="icon-btn" title="关闭" aria-label="关闭" onClick={close}>
            <Icon name="x" size={16} />
          </button>
        </header>

        {isCode ? (
          <div className="alert-hero">
            <div className="alert-code" id={heroId}>
              {code}
            </div>
            <button
              ref={primaryRef}
              className="btn btn-sm alert-copy"
              title="复制验证码"
              onClick={() => void copy()}
            >
              <Icon name="copy" size={14} />
              复制
            </button>
          </div>
        ) : (
          <p className="alert-summary" id={heroId}>
            {alert.summary || alert.subject}
          </p>
        )}

        <dl className="alert-meta">
          <div className="alert-meta-row">
            <dt>发件人</dt>
            <dd>{alert.from}</dd>
          </div>
          <div className="alert-meta-row">
            <dt>主题</dt>
            <dd>{alert.subject}</dd>
          </div>
          <div className="alert-meta-row">
            <dt>账户</dt>
            <dd className="alert-mono">{alert.accountEmail}</dd>
          </div>
        </dl>

        {queued > 0 && (
          <div className="alert-queued">
            <Icon name="bell" size={13} />
            还有 {queued} 条提醒
          </div>
        )}

        <footer className="alert-actions">
          {isCode && (
            <button className="btn btn-primary" onClick={() => void copyAndClose()}>
              <Icon name="copy" size={15} />
              复制并关闭
            </button>
          )}
          <button
            ref={isCode ? null : primaryRef}
            className={isCode ? "btn" : "btn btn-primary"}
            onClick={() => void openAlertMessage(alert)}
          >
            查看邮件
          </button>
          <button className="btn btn-ghost" onClick={close}>
            关闭
          </button>
        </footer>
      </div>
    </div>
  );
}
