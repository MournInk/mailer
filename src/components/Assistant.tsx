/**
 * The assistant — ask the mailbox questions in plain language.
 *
 * Everything it can do already exists in the backend (retrieval over the
 * embedding index, memory, the shared tool layer); this is the door.
 *
 * It floats over the app instead of taking a column in it: as a fourth pane it
 * squeezed the reading pane until long subjects wrapped one character per line,
 * and a conversation about your mail is something you dip into and dismiss, not
 * a permanent third of the window. Closed, it collapses to a launcher in the
 * corner, the way a site's chat widget does.
 *
 * Two things are deliberate:
 *  - Sending mail is never done on the model's word. When the assistant wants
 *    to send, the backend hands back a PendingAction and this panel renders it
 *    as a draft the user reads and approves. Declining just drops it.
 *  - Citations are shown, not hidden. An answer about someone's mail is worth
 *    little if you cannot check which messages it came from, so every cited
 *    message is clickable straight into the reading pane.
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import * as api from "../lib/api";
import { renderRichText } from "../lib/richText";
import { formatDate, useApp } from "../lib/store";
import type { ChatTurn, PendingAction, SearchHit } from "../lib/types";
import { Icon } from "./Icon";
import "./Assistant.css";

/** Openers shown on an empty conversation, phrased as a user would type them. */
const STARTERS = [
  "这周有哪些账单邮件？",
  "总结一下今天收到的重要邮件",
  "我最近和谁邮件往来最多？",
  "找出所有还没处理的验证码",
];

export function Assistant() {
  const { assistantOpen, setAssistantOpen, select, pushToast } = useApp();

  const [conversationId, setConversationId] = useState<string | null>(null);
  const [turns, setTurns] = useState<ChatTurn[]>([]);
  const [input, setInput] = useState("");
  const [busy, setBusy] = useState(false);
  const [pending, setPending] = useState<PendingAction | null>(null);
  /** The answer as it is being written, before the finished turn arrives. */
  const [draft, setDraft] = useState("");

  // The conversation this panel is currently waiting on. A ref because the
  // listener is registered once and must see the current value, not the one
  // captured when it was set up.
  const askingRef = useRef<string | null>(null);

  const scrollRef = useRef<HTMLDivElement | null>(null);
  const inputRef = useRef<HTMLTextAreaElement | null>(null);

  // Follow the conversation as it grows — including while an answer is being
  // written, which is the whole point of streaming it.
  useEffect(() => {
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [turns, busy, pending, draft]);

  // Fragments of the answer, pushed from the backend as the model writes them.
  // Registered once for the life of the panel; `askingRef` decides what to keep.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void api
      .onAssistantDelta((d) => {
        // A new conversation's id is minted by the backend, so the first answer
        // of a thread arrives tagged with an id this panel has not seen yet.
        // Anything while we are waiting is ours; anything else is not.
        if (askingRef.current === null) return;
        if (askingRef.current !== "" && d.conversationId !== askingRef.current) return;
        setDraft((t) => t + d.text);
      })
      .then((u) => {
        unlisten = u;
      })
      .catch(() => {});
    return () => unlisten?.();
  }, []);

  useEffect(() => {
    if (assistantOpen) inputRef.current?.focus();
  }, [assistantOpen]);

  // Grow with the text, up to the cap the stylesheet sets, then scroll. A
  // textarea has no intrinsic sizing, so this is the only way to have one line
  // occupy one line.
  useEffect(() => {
    const el = inputRef.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${el.scrollHeight}px`;
  }, [input]);

  const ask = useCallback(
    async (text: string) => {
      const question = text.trim();
      if (!question || busy) return;

      // Show the question immediately; a round trip can take seconds and an
      // input that empties into nothing feels broken.
      const optimistic: ChatTurn = {
        id: `local-${Date.now()}`,
        conversationId: conversationId ?? "",
        role: "user",
        content: question,
        toolCalls: [],
        citations: [],
        reasoning: null,
        createdAt: Date.now(),
      };
      setTurns((t) => [...t, optimistic]);
      setInput("");
      setBusy(true);
      setDraft("");
      askingRef.current = conversationId ?? "";
      try {
        const reply = await api.assistantAsk(conversationId, question);
        setConversationId(reply.turn.conversationId);
        // Replace rather than keep: the streamed text may include prose from a
        // round that then called a tool, and the stored turn is the answer.
        setTurns((t) => [...t, reply.turn]);
        setPending(reply.pendingConfirmation ?? null);
      } catch (e) {
        // Drop the optimistic turn: leaving it implies the question was asked.
        setTurns((t) => t.filter((x) => x.id !== optimistic.id));
        setInput(question);
        pushToast("error", `助手出错: ${e}`);
      } finally {
        askingRef.current = null;
        setDraft("");
        setBusy(false);
      }
    },
    [busy, conversationId, pushToast],
  );

  const confirmSend = useCallback(async () => {
    if (!pending) return;
    setBusy(true);
    try {
      await api.confirmPendingAction(pending.id);
      setPending(null);
      pushToast("ok", "邮件已发送");
    } catch (e) {
      pushToast("error", `发送失败: ${e}`);
    } finally {
      setBusy(false);
    }
  }, [pending, pushToast]);

  const reset = useCallback(() => {
    setConversationId(null);
    setTurns([]);
    setPending(null);
    setInput("");
    inputRef.current?.focus();
  }, []);

  // Closed, the assistant is a launcher parked in the corner — the live-chat
  // convention, and the one place on screen nothing else competes for.
  if (!assistantOpen) {
    return (
      <button
        className="asst-launcher"
        onClick={() => setAssistantOpen(true)}
        title="AI 助手（Ctrl/⌘ + J）"
        aria-label="打开 AI 助手"
      >
        <Icon name="spark" size={20} />
        {turns.length > 0 && <span className="asst-launcher-dot" aria-hidden />}
      </button>
    );
  }

  return (
    <aside className="asst" aria-label="AI 助手">
      <header className="asst-head">
        <span className="asst-mark" aria-hidden>
          <Icon name="spark" size={14} />
        </span>
        <h2 className="asst-title">AI 助手</h2>
        <div className="asst-head-actions">
          <button
            className="icon-btn"
            onClick={reset}
            disabled={busy || turns.length === 0}
            title="开始新对话"
            aria-label="开始新对话"
          >
            <Icon name="plus" size={16} />
          </button>
          <button
            className="icon-btn"
            onClick={() => setAssistantOpen(false)}
            title="收起助手"
            aria-label="收起助手"
          >
            <Icon name="chevron-down" size={16} />
          </button>
        </div>
      </header>

      <div className="asst-scroll" ref={scrollRef}>
        {turns.length === 0 && !busy ? (
          <div className="asst-intro">
            <p className="asst-intro-lede">
              用日常说法向你的邮箱提问，它会检索相关邮件后回答，并附上引用来源。
            </p>
            <div className="asst-starters">
              {STARTERS.map((s) => (
                <button key={s} className="asst-starter" onClick={() => void ask(s)}>
                  {s}
                </button>
              ))}
            </div>
          </div>
        ) : (
          turns.map((t) => <Turn key={t.id} turn={t} onOpen={select} />)
        )}

        {/* The answer as it is written. Rendered as plain text with a cursor
            rather than as Markdown: half a fence or half a table is not valid
            Markdown, and re-parsing it on every fragment would make the panel
            flicker between interpretations. The finished turn is rendered. */}
        {busy && draft && (
          <div className="asst-turn">
            <div className="asst-stream">{draft}</div>
          </div>
        )}

        {busy && (
          <p className="asst-thinking">
            <Icon name="loader" size={14} className="asst-spin" />
            {draft ? "正在作答…" : "正在查阅邮件…"}
          </p>
        )}

        {pending && (
          <PendingDraft
            action={pending}
            busy={busy}
            onConfirm={confirmSend}
            onDiscard={() => setPending(null)}
          />
        )}
      </div>

      {/* One rounded field with the button inside it, growing with the text up to
          a few lines. The two-box version put a fixed 2-row textarea beside a
          square button, so a one-line question sat in a box built for two and a
          long one scrolled inside three. */}
      <form
        className="asst-composer"
        onSubmit={(e) => {
          e.preventDefault();
          void ask(input);
        }}
      >
        <div className="asst-field">
          <textarea
            ref={inputRef}
            className="asst-input"
            value={input}
            rows={1}
            placeholder="问点什么…"
            disabled={busy}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                void ask(input);
              }
            }}
          />
          <button
            type="submit"
            className="asst-send"
            disabled={busy || !input.trim()}
            title="发送（Enter，Shift+Enter 换行）"
            aria-label="发送"
          >
            <Icon name={busy ? "loader" : "send"} size={15} className={busy ? "mv-spin" : undefined} />
          </button>
        </div>
      </form>
    </aside>
  );
}

function Turn({ turn, onOpen }: { turn: ChatTurn; onOpen: (id: string) => void }) {
  if (turn.role === "user") {
    return (
      <div className="asst-turn asst-turn-user">
        <p className="asst-bubble">{turn.content}</p>
      </div>
    );
  }

  return (
    <div className="asst-turn asst-turn-assistant">
      {turn.reasoning && <Reasoning text={turn.reasoning} />}

      {turn.toolCalls.length > 0 && (
        <ul className="asst-tools" aria-label="助手执行的操作">
          {turn.toolCalls.map((c, i) => (
            <li key={`${c.name}-${i}`} className={c.ok ? "" : "failed"}>
              <Icon name={c.ok ? "check" : "alert"} size={12} />
              <span className="asst-tool-name">{c.name}</span>
              <span className="asst-tool-summary">{c.summary}</span>
            </li>
          ))}
        </ul>
      )}

      <Answer text={turn.content} />

      {turn.citations.length > 0 && <Citations hits={turn.citations} onOpen={onOpen} />}
    </div>
  );
}

/**
 * The answer itself, as Markdown and LaTeX.
 *
 * `renderRichText` is the only thing allowed to produce this markup, and it
 * sanitizes on the way out — the text is written by a model that has been
 * reading mail from strangers.
 */
function Answer({ text }: { text: string }) {
  const html = useMemo(() => renderRichText(text), [text]);
  return (
    <div
      className="asst-answer"
      dangerouslySetInnerHTML={{ __html: html }}
    />
  );
}

/**
 * The model's chain of thought, collapsed by default. Worth keeping — it is how
 * you tell a reasoned answer from a guessed one — but not worth reading every
 * time, so it stays out of the way until asked for.
 */
function Reasoning({ text }: { text: string }) {
  const [open, setOpen] = useState(false);
  return (
    <div className={`asst-think${open ? " open" : ""}`}>
      <button className="asst-think-toggle" onClick={() => setOpen(!open)}>
        <Icon name={open ? "chevron-down" : "chevron-right"} size={13} />
        <span>思考过程</span>
      </button>
      {open && <pre className="asst-think-body">{text}</pre>}
    </div>
  );
}

function Citations({ hits, onOpen }: { hits: SearchHit[]; onOpen: (id: string) => void }) {
  return (
    <div className="asst-cites">
      <p className="asst-cites-label">引用邮件</p>
      {hits.map((h) => (
        <button
          key={h.messageId}
          className="asst-cite"
          onClick={() => void onOpen(h.messageId)}
          title="在阅读窗格中打开"
        >
          <span className="asst-cite-top">
            <span className="asst-cite-from">{h.fromName || h.fromAddr}</span>
            <span className="asst-cite-date">{formatDate(h.date)}</span>
          </span>
          <span className="asst-cite-subject">{h.subject || "(无主题)"}</span>
          {h.excerpt && <span className="asst-cite-excerpt">{h.excerpt}</span>}
        </button>
      ))}
    </div>
  );
}

/**
 * A draft the assistant wants to send. It is rendered in full — recipients,
 * subject and body — because approving something you cannot read is not
 * approval.
 */
function PendingDraft({
  action,
  busy,
  onConfirm,
  onDiscard,
}: {
  action: PendingAction;
  busy: boolean;
  onConfirm: () => void;
  onDiscard: () => void;
}) {
  const p = action.payload as {
    to?: string[];
    subject?: string;
    body?: string;
  };

  return (
    <section className="asst-draft" aria-label="待确认的邮件">
      <header className="asst-draft-head">
        <Icon name="send" size={14} />
        <span>助手想发送这封邮件，需要你确认</span>
      </header>
      <dl className="asst-draft-fields">
        <dt>收件人</dt>
        <dd>{(p.to ?? []).join("、") || "—"}</dd>
        <dt>主题</dt>
        <dd>{p.subject || "(无主题)"}</dd>
      </dl>
      <pre className="asst-draft-body">{p.body || ""}</pre>
      <div className="asst-draft-actions">
        <button className="btn btn-primary btn-sm" onClick={onConfirm} disabled={busy}>
          确认发送
        </button>
        <button className="btn btn-sm" onClick={onDiscard} disabled={busy}>
          放弃
        </button>
      </div>
    </section>
  );
}
