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
import {
  clipboardItems,
  ContextMenu,
  SEP,
  useContextMenu,
  type MenuItem,
} from "./ContextMenu";
import { OverlayScroll, type OverlayScrollHandle } from "./OverlayScroll";
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
    markRead,
    remove,
    openCompose,
  } = useApp();

  // Multi-select: Ctrl/Cmd-click adds one, Shift-click takes a range from the
  // last clicked row. `anchor` is that row, kept separately from the cursor so
  // arrow-key navigation does not move the range origin.
  const [picked, setPicked] = useState<Set<string>>(() => new Set());
  const [anchor, setAnchor] = useState<string | null>(null);
  // Row menus only. The clipboard fallback is mounted once at the app shell, so
  // it covers the settings and onboarding screens too.
  const { state: menu, close: closeMenu, openAt } = useContextMenu({
    clipboardFallback: false,
  });

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

  // One lookup for the whole render instead of a linear scan per row.
  const accountEmail = useMemo(
    () => new Map(accounts.map((a) => [a.id, a.email])),
    [accounts],
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

  /** Ctrl/Cmd-click toggles one row; Shift-click extends from the anchor. */
  const onRowClick = useCallback(
    (id: string, e: React.MouseEvent) => {
      const additive = e.ctrlKey || e.metaKey;
      const ranged = e.shiftKey;

      if (!additive && !ranged) {
        setPicked(new Set());
        setAnchor(id);
        void select(id);
        return;
      }

      const ids = page.items.map((m) => m.id);
      setPicked((prev) => {
        const next = new Set(prev);
        if (ranged && anchor) {
          const a = ids.indexOf(anchor);
          const b = ids.indexOf(id);
          if (a >= 0 && b >= 0) {
            for (const mid of ids.slice(Math.min(a, b), Math.max(a, b) + 1)) next.add(mid);
            return next;
          }
        }
        if (next.has(id)) next.delete(id);
        else next.add(id);
        return next;
      });
      // A range keeps its origin; a toggle becomes the new origin.
      if (!ranged) setAnchor(id);
    },
    [anchor, page.items, select],
  );

  const clearPicked = useCallback(() => setPicked(new Set()), []);

  const bulk = useCallback(
    async (fn: (ids: string[]) => Promise<void>) => {
      const ids = [...picked];
      if (ids.length === 0) return;
      await fn(ids);
      clearPicked();
    },
    [picked, clearPicked],
  );

  /** Menu for one row. Acts on the selection when the row is part of it. */
  const rowMenu = useCallback(
    (item: MessageHeader, e: React.MouseEvent) => {
      const target = picked.has(item.id) && picked.size > 1 ? [...picked] : [item.id];
      const many = target.length > 1;
      const items: MenuItem[] = [
        {
          id: "open",
          label: "打开",
          icon: "mail",
          disabled: many,
          run: () => void select(item.id),
        },
        {
          id: "reply",
          label: "回复",
          icon: "reply",
          disabled: many,
          run: () =>
            openCompose({
              accountId: item.accountId,
              to: item.fromAddr,
              subject: item.subject.startsWith("Re:") ? item.subject : `Re: ${item.subject}`,
              inReplyTo: item.id,
            }),
        },
        SEP,
        {
          id: "read",
          label: many ? `标记 ${target.length} 封为已读` : "标记为已读",
          icon: "check",
          run: () => void markRead(target, true),
        },
        {
          id: "unread",
          label: many ? `标记 ${target.length} 封为未读` : "标记为未读",
          icon: "mail",
          run: () => void markRead(target, false),
        },
        {
          id: "star",
          label: item.starred ? "取消星标" : "加星标",
          icon: "star",
          disabled: many,
          run: () => void toggleStar(item.id, !item.starred),
        },
        SEP,
      ];

      if (item.verificationCode) {
        items.push({
          id: "copy-code",
          label: `复制验证码 ${item.verificationCode}`,
          icon: "key",
          run: () => copyCode(item.verificationCode!),
        });
      }
      items.push(
        {
          id: "copy-from",
          label: "复制发件人地址",
          icon: "copy",
          run: async () => {
            await navigator.clipboard.writeText(item.fromAddr);
            pushToast("ok", "已复制发件人地址");
          },
        },
        SEP,
        {
          // Same meaning as the button in the reading pane and the one in the
          // selection bar: gone here and gone on the server. The label says so,
          // because a "删除" that only hid the mail locally is what the old
          // dropdown offered and nobody should have to remember which is which.
          id: "delete",
          label: many ? `删除 ${target.length} 封（含服务器）` : "删除（含服务器）",
          icon: "trash",
          danger: true,
          run: async () => {
            await remove(target);
            clearPicked();
          },
        },
      );

      // Clipboard actions still belong here when text happens to be selected.
      const clip = clipboardItems(e.target).filter((c) => !c.disabled);
      if (clip.length) items.push(SEP, ...clip);

      openAt(e, items);
    },
    [
      clearPicked, copyCode, markRead, openAt, openCompose, picked, pushToast,
      remove, select, toggleStar,
    ],
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
  const scrollRef = useRef<OverlayScrollHandle | null>(null);
  /** Offset we already asked for — stops a stalled page from looping. */
  const attempted = useRef(-1);

  useEffect(() => {
    attempted.current = -1;
  }, [filter]);

  const maybeLoadMore = useCallback(() => {
    const el = scrollRef.current?.el;
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
          <span className="ml-scope-lead">
            <span className="ml-scope-name">{scope}</span>
            {scopeHint && <span className="ml-scope-hint">{scopeHint}</span>}
          </span>
          <span className="ml-scope-count">{page.total} 封</span>
          {page.unread > 0 && (
            <span className="ml-scope-unread">{page.unread} 未读</span>
          )}
          {/* The app's sync control, next to the count it changes. It follows
              the current scope: one account when the list is filtered to one,
              every account otherwise. */}
          <button
            className="icon-btn ml-refresh"
            onClick={() => void sync(filter.accountId)}
            disabled={syncing}
            aria-label={syncing ? "正在同步" : "立即收取新邮件"}
            title={
              syncing
                ? "正在同步…"
                : filter.accountId
                  ? `立即收取「${accountLabel ?? "该账户"}」的新邮件`
                  : "立即收取全部账户的新邮件"
            }
          >
            <Icon name="refresh" size={15} className={syncing ? "ml-spin" : undefined} />
          </button>
        </div>
      </header>

      {picked.size > 0 && (
        <div className="ml-selbar" role="toolbar" aria-label="批量操作">
          <span className="ml-selbar-count">已选 {picked.size} 封</span>
          <button className="btn" onClick={() => void bulk((ids) => markRead(ids, true))}>
            <Icon name="check" size={14} />
            标记已读
          </button>
          <button className="btn" onClick={() => void bulk((ids) => markRead(ids, false))}>
            标记未读
          </button>
          <button
            className="btn btn-danger"
            onClick={() => void bulk((ids) => remove(ids))}
          >
            <Icon name="trash" size={14} />
            删除
          </button>
          <button className="icon-btn" onClick={clearPicked} aria-label="取消选择">
            <Icon name="x" size={15} />
          </button>
        </div>
      )}

      <OverlayScroll
        className="ml-scroll"
        handle={scrollRef}
        onScroll={maybeLoadMore}
        onKeyDown={onKeyDown}
        tabIndex={0}
        role="listbox"
        aria-label="邮件列表"
      >
        <div className="ml-rows">
        {showSkeleton ? (
          <div className="ml-skeletons" aria-hidden>
            {Array.from({ length: SKELETON_ROWS }, (_, i) => (
              <div className="ml-skel-row" key={i}>
                <div className="ml-skel-line">
                  <div className="ml-skel-bar ml-skel-from" />
                </div>
                <div className="ml-skel-line">
                  <div className="ml-skel-bar ml-skel-subject" />
                </div>
                <div className="ml-skel-line">
                  <div className="ml-skel-bar ml-skel-preview" />
                </div>
              </div>
            ))}
          </div>
        ) : items.length === 0 ? (
          <div className="empty-state ml-empty fade-up">
            <span className="ml-empty-icon">
              <Icon name={filtered ? "search" : "inbox"} size={20} />
            </span>
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
                picked={picked.has(m.id)}
                /* Which mailbox took delivery. Shown whenever the list is not
                   already narrowed to one account — including with a single
                   account configured, where it used to be hidden and the answer
                   to "which address is this going to" was nowhere on screen. */
                showAccount={!filter.accountId}
                accountLabel={accountEmail.get(m.accountId) ?? ""}
                onRowClick={onRowClick}
                onContextMenu={rowMenu}
                registerRow={registerRow}
              />
            ))}
            {loadingMore && (
              <div className="ml-more">
                <Icon name="loader" size={13} className="ml-spin" />
                加载中…
              </div>
            )}
          </>
        )}
        </div>
      </OverlayScroll>

      <ContextMenu state={menu} onClose={closeMenu} />
    </section>
  );
}

/**
 * One list row. Three fixed line boxes — sender + date, subject + category,
 * summary — so read and unread rows share a baseline grid, plus an optional
 * fourth line for the verification code. Every affordance has one home:
 * attachment before the date, category at the right of the subject, star in
 * its own rail. Nothing moves as you scroll.
 */
function MessageRow({
  item,
  selected,
  cursor,
  onOpen,
  onToggleStar,
  onCopyCode,
  registerRow,
  picked,
  showAccount,
  accountLabel,
  onRowClick,
  onContextMenu,
}: {
  item: MessageHeader;
  selected: boolean;
  cursor: boolean;
  onOpen: (id: string) => void;
  onToggleStar: (id: string, starred: boolean) => Promise<void>;
  onCopyCode: (code: string) => Promise<void>;
  registerRow: (id: string, el: HTMLDivElement | null) => void;
  picked: boolean;
  /** Which mailbox received it — shown only when the list spans accounts. */
  showAccount: boolean;
  accountLabel: string;
  onRowClick: (id: string, e: React.MouseEvent) => void;
  onContextMenu: (item: MessageHeader, e: React.MouseEvent) => void;
}) {
  const sender = item.fromName || item.fromAddr;
  const preview = item.summary || item.snippet;
  const cls = [
    "ml-row",
    item.unread ? "unread" : "read",
    selected ? "selected" : "",
    cursor ? "is-cursor" : "",
    picked ? "picked" : "",
  ]
    .filter(Boolean)
    .join(" ");

  return (
    <div
      className={cls}
      ref={(el) => registerRow(item.id, el)}
      onClick={(e) => {
        // A modifier means "change the selection", not "open".
        if (e.ctrlKey || e.metaKey || e.shiftKey) onRowClick(item.id, e);
        else onOpen(item.id);
      }}
      onContextMenu={(e) => onContextMenu(item, e)}
      role="option"
      aria-selected={selected || picked}
    >
      <span className="ml-row-rail" aria-hidden>
        <span className="ml-row-dot" />
      </span>

      <div className="ml-row-body">
        <div className="ml-line">
          <span className="ml-from">{sender}</span>
          {showAccount && accountLabel && (
            <span className="ml-acct" title={`收件邮箱 ${accountLabel}`}>
              {accountLabel}
            </span>
          )}
          {item.hasAttachments && (
            <Icon name="paperclip" size={12} className="ml-clip" />
          )}
          <span className="ml-date">{formatDate(item.date)}</span>
        </div>

        <div className="ml-line">
          <span className="ml-subject">{item.subject || "(无主题)"}</span>
          {item.threadCount > 1 && (
            <span className="ml-thread" title={`这个会话有 ${item.threadCount} 封邮件`}>
              {item.threadCount}
            </span>
          )}
          {item.category && (
            <span className={`badge badge-${item.category} ml-cat`}>
              {CATEGORY_LABEL[item.category]}
            </span>
          )}
        </div>

        <p className="ml-preview">{preview}</p>

        {item.verificationCode && (
          <div className="ml-code-line">
            <button
              className="ml-code"
              onClick={(e) => {
                e.stopPropagation();
                void onCopyCode(item.verificationCode!);
              }}
              title="点击复制验证码"
              aria-label={`复制验证码 ${item.verificationCode}`}
            >
              <Icon name="key" size={11} className="ml-code-key" />
              <span className="ml-code-text">{item.verificationCode}</span>
              <Icon name="copy" size={11} className="ml-code-copy" />
            </button>
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
