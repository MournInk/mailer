/**
 * Right pane: the reading surface for the selected message.
 *
 * Two things here are load-bearing beyond layout:
 *  - HTML mail is hostile input. Everything goes through `sanitizeBody()`
 *    (DOMPurify) before it can reach `dangerouslySetInnerHTML`, and remote
 *    images are parked in `data-src` until the user asks for them, because a
 *    remote image in mail is a read receipt for the sender.
 *  - Deleting means deleting: one button, and the message goes from here and
 *    from the server. The row disappears before the round trip finishes and
 *    comes back if the server refused it, which is a truer safety net than a
 *    confirmation dialog nobody reads.
 */

import { useCallback, useMemo, useState } from "react";
import DOMPurify from "dompurify";
import * as api from "../lib/api";
import { formatFullDate, useApp } from "../lib/store";
import { CATEGORY_LABEL, type AttachmentMeta, type EmailMessage } from "../lib/types";
import { Icon } from "./Icon";
import "./MessageView.css";

/** Attributes that would pull a remote resource the moment we render it. */
const REMOTE_ATTRS = ["src", "poster"] as const;

/**
 * DOMPurify profile for message bodies. Scripting, embedded documents, forms
 * and author stylesheets are dropped outright; `on*` handlers are never in
 * DOMPurify's allow-list to begin with, but they are named here so the intent
 * survives future edits.
 */
const SANITIZE_CONFIG = {
  FORBID_TAGS: [
    "script",
    "iframe",
    "object",
    "embed",
    "form",
    "style",
    "link",
    "base",
    "meta",
    "input",
    "button",
    "select",
    "textarea",
    "noscript",
  ],
  FORBID_ATTR: [
    "srcset",
    "background",
    "ping",
    "formaction",
    "onload",
    "onerror",
    "onclick",
    "onmouseover",
  ],
  ALLOW_DATA_ATTR: true,
  WHOLE_DOCUMENT: false,
};

interface CleanBody {
  html: string;
  /** How many remote resources we neutralised — drives the "显示图片" banner. */
  blocked: number;
}

/** `url(...)` references inside inline styles are trackers too. */
const CSS_URL = /url\(\s*(['"]?)(?!data:)[^)]*\1\s*\)/gi;

/**
 * Sanitize one message body. With `allowRemote` false every remote reference is
 * moved to `data-src` (or stripped, for CSS) and counted; flipping it to true
 * re-runs the same pass over the original source so the images come back.
 */
function sanitizeBody(dirty: string, allowRemote: boolean): CleanBody {
  let blocked = 0;

  const hook = (node: Element) => {
    // External links must not get a handle on the opener window.
    if (node.tagName === "A") {
      node.setAttribute("target", "_blank");
      node.setAttribute("rel", "noopener noreferrer");
    }

    if (allowRemote) return;

    for (const attr of REMOTE_ATTRS) {
      const value = node.getAttribute(attr);
      // `data:` and `cid:` payloads are local — only the network ones track.
      if (!value || value.startsWith("data:") || value.startsWith("cid:")) continue;
      node.setAttribute(`data-${attr}`, value);
      node.removeAttribute(attr);
      blocked += 1;
    }

    const style = node.getAttribute("style");
    if (style && CSS_URL.test(style)) {
      // `test` on a /g regex is stateful — reset before reusing it below.
      CSS_URL.lastIndex = 0;
      node.setAttribute("style", style.replace(CSS_URL, "none"));
      blocked += 1;
    }
    CSS_URL.lastIndex = 0;
  };

  DOMPurify.addHook("afterSanitizeAttributes", hook);
  try {
    return { html: DOMPurify.sanitize(dirty, SANITIZE_CONFIG), blocked };
  } finally {
    DOMPurify.removeHook("afterSanitizeAttributes", hook);
  }
}

/** Bytes → short human string (attachment rows are metadata, keep them terse). */
function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const kb = bytes / 1024;
  if (kb < 1024) return `${kb.toFixed(kb < 10 ? 1 : 0)} KB`;
  const mb = kb / 1024;
  if (mb < 1024) return `${mb.toFixed(mb < 10 ? 1 : 0)} MB`;
  return `${(mb / 1024).toFixed(1)} GB`;
}

/**
 * A short, uppercase kind label for the attachment chip — the extension when
 * the filename has a plausible one, otherwise the MIME subtype.
 */
function fileKind(att: AttachmentMeta): string {
  const dot = att.filename.lastIndexOf(".");
  const ext = dot > 0 ? att.filename.slice(dot + 1) : "";
  if (ext && ext.length <= 5) return ext.toUpperCase();
  const sub = att.mime.split("/")[1] || att.mime;
  return sub ? sub.slice(0, 5).toUpperCase() : "文件";
}

export function MessageView() {
  const { selected } = useApp();

  return (
    <section
      className={`mv-pane message-view-pane${selected ? "" : " hidden-mobile"}`}
      aria-label="邮件详情"
    >
      {selected ? (
        /* keyed by id so disclosures, the image gate and menus reset per mail */
        <MessageDetail key={selected.id} msg={selected} />
      ) : (
        <div className="empty-state mv-empty fade-up">
          <span className="mv-empty-mark">
            <Icon name="mail" size={21} />
          </span>
          <p className="empty-title mv-empty-title">选择一封邮件</p>
          <p className="mv-empty-hint">
            正文、附件与 AI 摘要都会显示在这里。
          </p>
        </div>
      )}
    </section>
  );
}

function MessageDetail({ msg }: { msg: EmailMessage }) {
  const { select, toggleStar, remove, openCompose, pushToast } = useApp();

  const [showImages, setShowImages] = useState(false);
  const [showRecipients, setShowRecipients] = useState(false);
  const [showReason, setShowReason] = useState(false);
  const [reclassifying, setReclassifying] = useState(false);

  const subject = msg.subject || "(无主题)";
  const analysis = msg.analysis;
  /** Monogram for the letterhead — same convention as the sidebar accounts. */
  const monogram = (msg.fromName || msg.fromAddr).trim().charAt(0) || "?";

  const body = useMemo(
    () => (msg.bodyHtml ? sanitizeBody(msg.bodyHtml, showImages) : null),
    [msg.bodyHtml, showImages],
  );

  // -- actions ---------------------------------------------------------------
  const reply = useCallback(() => {
    openCompose({
      accountId: msg.accountId,
      to: msg.fromAddr,
      subject: subject.startsWith("Re:") ? subject : `Re: ${subject}`,
      inReplyTo: msg.id,
    });
  }, [openCompose, msg.accountId, msg.fromAddr, msg.id, subject]);

  const reclassify = useCallback(async () => {
    if (reclassifying) return;
    setReclassifying(true);
    try {
      const result = await api.reclassify(msg.id);
      // pull the persisted analysis back into the pane
      await select(msg.id);
      pushToast("ok", `已重新分类为「${CATEGORY_LABEL[result.category]}」`);
    } catch (e) {
      pushToast("error", `重新分类失败: ${e}`);
    } finally {
      setReclassifying(false);
    }
  }, [reclassifying, msg.id, select, pushToast]);

  const copyCode = useCallback(
    async (code: string) => {
      try {
        await navigator.clipboard.writeText(code);
        pushToast("ok", "验证码已复制");
      } catch {
        pushToast("error", "复制失败，请手动选择");
      }
    },
    [pushToast],
  );

  return (
    <>
      <header className="mv-bar">
        <button
          className="icon-btn mv-back"
          onClick={() => void select(null)}
          title="返回列表"
          aria-label="返回列表"
        >
          <Icon name="back" size={17} />
        </button>

        <span className="mv-bar-spacer" />

        {/* one control group rather than a row of loose buttons */}
        <div className="mv-actions">
          <button
            className={`icon-btn mv-act${msg.starred ? " active" : ""}`}
            onClick={() => void toggleStar(msg.id, !msg.starred)}
            title={msg.starred ? "取消星标" : "加星标"}
            aria-label={msg.starred ? "取消星标" : "加星标"}
            aria-pressed={msg.starred}
          >
            <Icon name="star" size={17} />
          </button>

          <span className="mv-act-sep" aria-hidden="true" />

          <button
            className="btn btn-ghost btn-sm mv-act"
            onClick={reply}
            title="回复发件人"
          >
            <Icon name="reply" size={15} />
            <span className="mv-bar-text">回复</span>
          </button>

          <button
            className="btn btn-ghost btn-sm mv-act"
            onClick={() => void reclassify()}
            disabled={reclassifying}
            title="让 AI 重新分析这封邮件"
          >
            <Icon
              name={reclassifying ? "loader" : "spark"}
              size={15}
              className={reclassifying ? "mv-spin" : undefined}
            />
            <span className="mv-bar-text">{reclassifying ? "分析中…" : "重新分类"}</span>
          </button>

          <span className="mv-act-sep" aria-hidden="true" />

          {/* One button, one meaning: the message is deleted, here and on the
              server. It disappears at once and comes back if the server
              refused — see `remove` in the store. */}
          <button
            className="btn btn-ghost btn-sm mv-act mv-del"
            onClick={() => void remove([msg.id])}
            title="删除邮件（同时从服务器删除）"
          >
            <Icon name="trash" size={15} />
            <span className="mv-bar-text">删除</span>
          </button>
        </div>
      </header>

      <div className="mv-scroll">
        <article className="mv-doc fade-up">
          {/* -- letterhead ------------------------------------------------- */}
          <header className="mv-head">
            <h1 className="mv-subject">{subject}</h1>

            <div className="mv-meta">
              <span className="mv-mono-mark" aria-hidden="true">
                {monogram}
              </span>
              <div className="mv-from">
                <span className="mv-from-name">{msg.fromName || msg.fromAddr}</span>
                <span className="mv-addr">{msg.fromAddr}</span>
              </div>
              <time className="mv-date" dateTime={new Date(msg.date).toISOString()}>
                {formatFullDate(msg.date)}
              </time>
            </div>

            {msg.toAddrs.length > 0 &&
              (msg.toAddrs.length <= 2 ? (
                <p className="mv-to">
                  <span className="mv-to-label">收件人</span>
                  <span className="mv-addr">{msg.toAddrs.join("、")}</span>
                </p>
              ) : (
                <div className="mv-to">
                  <button
                    className="mv-disclosure"
                    onClick={() => setShowRecipients((v) => !v)}
                    aria-expanded={showRecipients}
                  >
                    <Icon name={showRecipients ? "chevron-down" : "chevron-right"} size={13} />
                    收件人 {msg.toAddrs.length} 人
                  </button>
                  {showRecipients && (
                    <div className="mv-to-list">
                      {msg.toAddrs.map((addr) => (
                        <span className="mv-addr" key={addr}>
                          {addr}
                        </span>
                      ))}
                    </div>
                  )}
                </div>
              ))}
          </header>

          {/* -- AI panel: the signature element of the app ------------------ */}
          {analysis && (
            <section
              className={`mv-ai${analysis.verificationCode ? " mv-ai-code" : ""}`}
              aria-label="AI 分析"
            >
              <header className="mv-ai-head">
                <span className="mv-ai-chip" aria-hidden="true">
                  <Icon name="spark" size={14} />
                </span>
                <span className="mv-ai-label">AI 分析</span>
                <span className={`badge badge-${analysis.category}`}>
                  {CATEGORY_LABEL[analysis.category]}
                </span>
              </header>

              {analysis.verificationCode && (
                <div className="mv-code-block">
                  <span className="mv-code-label">验证码</span>
                  <div className="mv-code-row">
                    <span className="mv-code">{analysis.verificationCode}</span>
                    <button
                      className="btn btn-sm mv-code-copy"
                      onClick={() => void copyCode(analysis.verificationCode!)}
                      title="复制验证码"
                    >
                      <Icon name="copy" size={14} />
                      复制
                    </button>
                  </div>
                </div>
              )}

              {analysis.summary && <p className="mv-summary">{analysis.summary}</p>}

              <footer className="mv-ai-foot">
                <span className="mv-conf">
                  置信度
                  <span className="mv-conf-num">
                    {Math.round(analysis.confidence * 100)}%
                  </span>
                </span>
                {analysis.reason && (
                  <button
                    className="mv-disclosure"
                    onClick={() => setShowReason((v) => !v)}
                    aria-expanded={showReason}
                  >
                    <Icon name={showReason ? "chevron-down" : "chevron-right"} size={13} />
                    判断依据
                  </button>
                )}
              </footer>

              {analysis.reason && showReason && (
                <p className="mv-reason">{analysis.reason}</p>
              )}
            </section>
          )}

          {/* -- remote image gate ------------------------------------------- */}
          {body && body.blocked > 0 && !showImages && (
            <div className="card mv-imgbar">
              <span className="mv-imgbar-chip" aria-hidden="true">
                <Icon name="shield" size={16} />
              </span>
              <span className="mv-imgbar-text">
                <span className="mv-imgbar-title">
                  已阻止 {body.blocked} 处远程图片
                </span>
                <span className="mv-imgbar-hint">
                  载入远程图片会让发件人知道你读过这封邮件。
                </span>
              </span>
              <button
                className="btn btn-sm mv-imgbar-btn"
                onClick={() => setShowImages(true)}
              >
                显示图片
              </button>
            </div>
          )}

          {/* -- body --------------------------------------------------------- */}
          {body ? (
            <div
              className="mv-html"
              /* sanitizeBody() is the only path that produces this string */
              dangerouslySetInnerHTML={{ __html: body.html }}
            />
          ) : msg.bodyText ? (
            <div className="mv-text">{msg.bodyText}</div>
          ) : (
            <p className="mv-text mv-nobody">这封邮件没有正文内容。</p>
          )}

          {/* -- attachments --------------------------------------------------- */}
          {msg.attachments.length > 0 && (
            <section className="mv-atts" aria-label="附件">
              <div className="mv-atts-head">
                <span className="mv-atts-label">附件</span>
                <span className="mv-atts-count">{msg.attachments.length}</span>
              </div>
              <div className="mv-atts-grid">
                {msg.attachments.map((att, i) => (
                  <AttachmentRow key={`${att.filename}-${i}`} att={att} />
                ))}
              </div>
            </section>
          )}
        </article>
      </div>
    </>
  );
}

/** Attachments are metadata only — there is no download command yet. */
function AttachmentRow({ att }: { att: AttachmentMeta }) {
  return (
    <div className="card mv-att" title={att.mime}>
      <span className="mv-att-chip" aria-hidden="true">
        <Icon name="paperclip" size={15} />
      </span>
      <span className="mv-att-text">
        <span className="mv-att-name">{att.filename || "(未命名附件)"}</span>
        <span className="mv-att-meta">
          <span className="mv-att-kind">{fileKind(att)}</span>
          <span className="mv-att-size">{formatSize(att.size)}</span>
        </span>
      </span>
    </div>
  );
}
