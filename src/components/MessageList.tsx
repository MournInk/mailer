/**
 * Middle column: debounced search, the active scope header and the message
 * rows themselves. This is the densest surface in the app — rows are keyboard
 * navigable, the list pages in on scroll and every affordance (star, category,
 * verification code) is reachable without opening the mail.
 */

import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
} from "react";
import { formatDate, useApp } from "../lib/store";
import { CATEGORY_LABEL, type MessageHeader } from "../lib/types";
import { Icon } from "./Icon";
import "./MessageList.css";

/** Keystroke settling time before the search hits the backend. */
const SEARCH_DEBOUNCE = 250;
/** Distance (px) from the bottom that triggers the next page. */
const NEAR_BOTTOM = 260;
const SKELETON_ROWS = 6;

export function MessageList() {
  const {
    page,
    filter,
    accounts,
    syncMap,
    selectedId,
    loadingList,
    loadingMore,
    setFilter,
    select,
    toggleStar,
    loadMore,
    sync,
    pushToast,
  } = useApp();

  const items = page.items;

  // -- search (debounced so every keystroke does not hit the backend) --------
  const [query, setQuery] = useState(filter.search);

  // keep the box in sync when the filter is reset from elsewhere (sidebar)
  useEffect(() => {
    setQuery((q) => (q === filter.search ? q : filter.search));
  }, [filter.search]);

  useEffect(() => {
    if (query === filter.search) return;
    const t = window.setTimeout(() => setFilter({ search: query }), SEARCH_DEBOUNCE);
    return () => window.clearTimeout(t);
  }, [query, filter.search, setFilter]);

  const clearSearch = useCallback(() => {
    setQuery("");
    setFilter({ search: "" });
  }, [setFilter]);

  // -- scope line -----------------------------------------------------------
  const accountLabel = useMemo(
    () => accounts.find((a) => a.id === filter.accountId)?.label ?? null,
    [accounts, filter.accountId],
  );

  const scope = useMemo(() => {
    if (filter.category) return CATEGORY_LABEL[filter.category];
    if (filter.starredOnly) return "已加星标";
    if (filter.unreadOnly) return "未读邮件";
    return accountLabel ?? "全部邮件";
  }, [filter.category, filter.starredOnly, filter.unreadOnly, accountLabel]);

  // when a category/flag scope is combined with an account, show it as a hint
  const scopeHint =
    accountLabel &&
    (filter.category || filter.starredOnly || filter.unreadOnly)
      ? accountLabel
      : null;

  const syncing = useMemo(
    () =>
      Object.values(syncMap).some(
        (s) => s.phase !== "idle" && s.phase !== "error",
      ),
    [syncMap],
  );

  // -- keyboard cursor ------------------------------------------------------
  const [cursorId, setCursorId] = useState<string | null>(selectedId);
  const rowRefs = useRef(new Map<string, HTMLDivElement>());

  // follow selections made outside the list (alerts, deletions, …)
  useEffect(() => {
    if (selectedId) setCursorId(selectedId);
  }, [selectedId]);

  // keep the cursor row visible while arrowing through the list
  useEffect(() => {
    if (!cursorId) return;
    rowRefs.current.get(cursorId)?.scrollIntoView({ block: "nearest" });
  }, [cursorId]);

  const open = useCallback(
    (id: string) => {
      setCursorId(id);
      void select(id);
    },
    [select],
  );

  const onKeyDown = useCallback(
    (e: KeyboardEvent<HTMLDivElement>) => {
      if (items.length === 0) return;
      if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        e.preventDefault();
        const at = items.findIndex((m) => m.id === cursorId);
        const next =
          e.key === "ArrowDown"
            ? Math.min(items.length - 1, at < 0 ? 0 : at + 1)
            : Math.max(0, at < 0 ? 0 : at - 1);
        setCursorId(items[next].id);
      } else if (e.key === "Enter") {
        e.preventDefault();
        open(cursorId ?? items[0].id);
      }
    },
    [items, cursorId, open],
  );

  // -- infinite scroll ------------------------------------------------------
  const scrollRef = useRef<HTMLDivElement | null>(null);
  /** Offset we already asked for — stops a stalled page from looping. */
  const attempted = useRef(-1);

  useEffect(() => {
    attempted.current = -1;
  }, [filter]);

  const maybeLoadMore = useCallback(() => {
    const el = scrollRef.current;
    if (!el || loadingList || loadingMore) return;
    if (items.length >= page.total || attempted.current === items.length) return;
    if (el.scrollHeight - el.scrollTop - el.clientHeight > NEAR_BOTTOM) return;
    attempted.current = items.length;
    void loadMore();
  }, [items.length, page.total, loadingList, loadingMore, loadMore]);

  // also fires when a fresh page does not fill the viewport
  useEffect(() => {
    maybeLoadMore();
  }, [maybeLoadMore]);

  // -- verification code copy ----------------------------------------------
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

  const registerRow = useCallback((id: string, el: HTMLDivElement | null) => {
    if (el) rowRefs.current.set(id, el);
    else rowRefs.current.delete(id);
  }, []);

  const filtered =
    filter.search.trim().length > 0 ||
    filter.category !== null ||
    filter.starredOnly ||
    filter.unreadOnly;

  const showSkeleton = loadingList && items.length === 0;

  return (
    <section className="ml-pane">
      <header className="ml-header">
        <div className="ml-search">
          <Icon name="search" size={15} className="ml-search-icon" />
          <input
            className="input ml-search-input"
            type="search"
            placeholder="搜索发件人、主题或正文…"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            aria-label="搜索邮件"
          />
          {query.length > 0 && (
            <button
              className="icon-btn ml-search-clear"
              onClick={clearSearch}
              aria-label="清除搜索"
              title="清除搜索"
            >
              <Icon name="x" size={14} />
            </button>
          )}
        </div>

        <div className="ml-scope">
          <span className="ml-scope-name">{scope}</span>
          {scopeHint && <span className="ml-scope-hint">· {scopeHint}</span>}
          <span className="ml-scope-count">{page.total} 封</span>
          {page.unread > 0 && (
            <span className="ml-scope-unread">{page.unread} 未读</span>
          )}
          <button
            className="icon-btn ml-refresh"
            onClick={() => void sync(filter.accountId)}
            aria-label="立即同步"
            title="立即同步"
          >
            <Icon name="refresh" size={15} className={syncing ? "ml-spin" : undefined} />
          </button>
        </div>
      </header>

      <div
        className="ml-scroll"
        ref={scrollRef}
        onScroll={maybeLoadMore}
        onKeyDown={onKeyDown}
        tabIndex={0}
        role="listbox"
        aria-label="邮件列表"
      >
        {showSkeleton ? (
          <div className="ml-skeletons" aria-hidden>
            {Array.from({ length: SKELETON_ROWS }, (_, i) => (
              <div className="ml-skel-row" key={i}>
                <div className="ml-skel-bar ml-skel-from" />
                <div className="ml-skel-bar ml-skel-subject" />
                <div className="ml-skel-bar ml-skel-preview" />
              </div>
            ))}
          </div>
        ) : items.length === 0 ? (
          <div className="empty-state fade-up">
            <Icon name={filtered ? "search" : "inbox"} size={26} />
            <p className="empty-title">{filtered ? "没有匹配的邮件" : "收件箱是空的"}</p>
            <p className="ml-empty-hint">
              {filtered
                ? "换一个关键词，或清除当前的筛选条件。"
                : "点击上方的同步按钮，把邮箱里的邮件收取下来。"}
            </p>
          </div>
        ) : (
          <>
            {items.map((m) => (
              <MessageRow
                key={m.id}
                item={m}
                selected={m.id === selectedId}
                cursor={m.id === cursorId}
                onOpen={open}
                onToggleStar={toggleStar}
                onCopyCode={copyCode}
                registerRow={registerRow}
              />
            ))}
            {loadingMore && <div className="ml-more">加载中…</div>}
          </>
        )}
      </div>
    </section>
  );
}

/** One list row — three text lines plus badges, code chip and the star. */
function MessageRow({
  item,
  selected,
  cursor,
  onOpen,
  onToggleStar,
  onCopyCode,
  registerRow,
}: {
  item: MessageHeader;
  selected: boolean;
  cursor: boolean;
  onOpen: (id: string) => void;
  onToggleStar: (id: string, starred: boolean) => Promise<void>;
  onCopyCode: (code: string) => Promise<void>;
  registerRow: (id: string, el: HTMLDivElement | null) => void;
}) {
  const sender = item.fromName || item.fromAddr;
  const preview = item.summary || item.snippet;
  const cls = [
    "ml-row",
    item.unread ? "unread" : "read",
    selected ? "selected" : "",
    cursor ? "is-cursor" : "",
  ]
    .filter(Boolean)
    .join(" ");

  return (
    <div
      className={cls}
      ref={(el) => registerRow(item.id, el)}
      onClick={() => onOpen(item.id)}
      role="option"
      aria-selected={selected}
    >
      <span className="ml-row-dot" aria-hidden />

      <div className="ml-row-body">
        <div className="ml-line ml-line-top">
          <span className="ml-from">{sender}</span>
          <span className="ml-date">{formatDate(item.date)}</span>
        </div>

        <div className="ml-line">
          <span className="ml-subject">{item.subject || "(无主题)"}</span>
          {item.hasAttachments && (
            <Icon name="paperclip" size={13} className="ml-clip" />
          )}
        </div>

        {preview && <p className="ml-preview">{preview}</p>}

        {(item.category || item.verificationCode) && (
          <div className="ml-meta">
            {item.category && (
              <span className={`badge badge-${item.category}`}>
                {CATEGORY_LABEL[item.category]}
              </span>
            )}
            {item.verificationCode && (
              <button
                className="ml-code"
                onClick={(e) => {
                  e.stopPropagation();
                  void onCopyCode(item.verificationCode!);
                }}
                title="点击复制验证码"
                aria-label={`复制验证码 ${item.verificationCode}`}
              >
                <span className="ml-code-text">{item.verificationCode}</span>
                <Icon name="copy" size={11} />
              </button>
            )}
          </div>
        )}
      </div>

      <button
        className={`ml-star${item.starred ? " on" : ""}`}
        onClick={(e) => {
          e.stopPropagation();
          void onToggleStar(item.id, !item.starred);
        }}
        aria-label={item.starred ? "取消星标" : "加星标"}
        title={item.starred ? "取消星标" : "加星标"}
      >
        <Icon name="star" size={15} />
      </button>
    </div>
  );
}
