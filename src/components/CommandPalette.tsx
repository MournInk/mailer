/**
 * Command palette (Ctrl/Cmd+K).
 *
 * One entry point for everything the app can do, so nothing is reachable only
 * by hunting for a button. Commands are matched by a loose subsequence over
 * both the label and its keywords, which is what makes typing "yzm" find
 * 验证码 and "set" find 设置 — a strict substring match would find neither.
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useApp, type SettingsTab } from "../lib/store";
import type { Category } from "../lib/types";
import { Icon } from "./Icon";
import "./CommandPalette.css";

interface Command {
  id: string;
  label: string;
  hint?: string;
  icon: string;
  /** Extra text the matcher searches: pinyin initials, English, synonyms. */
  keywords: string;
  /** Shown right-aligned when this command also has a shortcut. */
  shortcut?: string;
  run: () => void;
}

/**
 * True when every character of `needle` appears in `haystack` in order.
 * Subsequence rather than substring: users type initials, not prefixes.
 */
export function fuzzyMatch(haystack: string, needle: string): boolean {
  if (!needle) return true;
  const h = haystack.toLowerCase();
  const n = needle.toLowerCase();
  let i = 0;
  for (const ch of h) {
    if (ch === n[i]) i += 1;
    if (i === n.length) return true;
  }
  return i === n.length;
}

export function CommandPalette() {
  const app = useApp();
  const {
    paletteOpen,
    setPaletteOpen,
    setAssistantOpen,
    setShortcutsOpen,
    openCompose,
    openSettings,
    setFilter,
    setTheme,
    sync,
    accounts,
    selectedId,
    remove,
    toggleStar,
    selected,
  } = app;

  const [query, setQuery] = useState("");
  const [cursor, setCursor] = useState(0);
  const inputRef = useRef<HTMLInputElement | null>(null);
  const listRef = useRef<HTMLDivElement | null>(null);

  const close = useCallback(() => {
    setPaletteOpen(false);
    setQuery("");
    setCursor(0);
  }, [setPaletteOpen]);

  const commands = useMemo<Command[]>(() => {
    const go = (category: Category | null, label: string, icon: string, kw: string): Command => ({
      id: `view-${category ?? "all"}`,
      label: `转到 ${label}`,
      icon,
      keywords: `goto view ${kw}`,
      run: () => setFilter({ category, starredOnly: false, unreadOnly: false }),
    });

    const list: Command[] = [
      {
        id: "compose",
        label: "写邮件",
        icon: "edit",
        keywords: "compose new write xyj",
        shortcut: "C",
        run: () => openCompose(),
      },
      {
        id: "assistant",
        label: "打开 AI 助手",
        hint: "对着收件箱提问",
        icon: "spark",
        keywords: "assistant ai ask chat zs",
        shortcut: "Ctrl+J",
        run: () => setAssistantOpen(true),
      },
      {
        id: "sync",
        label: "立即同步全部账户",
        icon: "refresh",
        keywords: "sync refresh fetch tb",
        run: () => void sync(null),
      },
      go(null, "全部邮件", "inbox", "all inbox qbyj"),
      go("verification", "验证码", "key", "verification code otp yzm"),
      go("important", "重要", "alert", "important zy"),
      go("normal", "普通", "mail", "normal pt"),
      go("spam", "垃圾邮件", "archive", "spam junk ljyj"),
      {
        id: "starred",
        label: "转到 星标",
        icon: "star",
        keywords: "starred flagged xb",
        run: () => setFilter({ category: null, starredOnly: true, unreadOnly: false }),
      },
      {
        id: "unread",
        label: "转到 未读",
        icon: "mail",
        keywords: "unread wd",
        run: () => setFilter({ category: null, starredOnly: false, unreadOnly: true }),
      },
      {
        id: "shortcuts",
        label: "键盘快捷键",
        icon: "code",
        keywords: "keyboard shortcuts help kjj",
        shortcut: "?",
        run: () => setShortcutsOpen(true),
      },
    ];

    // Message-scoped commands only make sense with something open.
    if (selectedId) {
      list.push(
        {
          id: "star",
          label: selected?.starred ? "取消星标" : "加星标",
          icon: "star",
          keywords: "star flag xb",
          shortcut: "S",
          run: () => void toggleStar(selectedId, !selected?.starred),
        },
        {
          id: "delete",
          label: "删除当前邮件",
          hint: "仅本地",
          icon: "trash",
          keywords: "delete remove sc",
          shortcut: "#",
          run: () => void remove([selectedId], false),
        },
      );
    }

    for (const a of accounts) {
      list.push({
        id: `acct-${a.id}`,
        label: `转到 ${a.label}`,
        hint: a.email,
        icon: "inbox",
        keywords: `account ${a.email} ${a.label}`,
        run: () => setFilter({ accountId: a.id }),
      });
    }

    const tab = (t: SettingsTab, label: string, kw: string): Command => ({
      id: `settings-${t}`,
      label: `设置 · ${label}`,
      icon: "settings",
      keywords: `settings ${kw}`,
      run: () => openSettings(t),
    });
    list.push(
      tab("accounts", "邮箱账户", "accounts mailbox zh"),
      tab("ai", "AI 过滤器", "ai model filter"),
      tab("channels", "通知渠道", "notify channels telegram bark"),
      tab("about", "关于", "about gy"),
    );

    for (const [t, label] of [
      ["light", "浅色"],
      ["dark", "深色"],
      ["system", "跟随系统"],
    ] as const) {
      list.push({
        id: `theme-${t}`,
        label: `主题 · ${label}`,
        icon: t === "light" ? "sun" : t === "dark" ? "moon" : "monitor",
        keywords: `theme appearance ${t} zt`,
        run: () => setTheme(t),
      });
    }

    return list;
  }, [
    accounts, openCompose, openSettings, remove, selected, selectedId,
    setAssistantOpen, setFilter, setShortcutsOpen, setTheme, sync, toggleStar,
  ]);

  const matches = useMemo(
    () => commands.filter((c) => fuzzyMatch(`${c.label} ${c.keywords}`, query.trim())),
    [commands, query],
  );

  // A shrinking result list must not strand the cursor past the end.
  useEffect(() => {
    setCursor((c) => Math.min(c, Math.max(0, matches.length - 1)));
  }, [matches.length]);

  useEffect(() => {
    if (paletteOpen) inputRef.current?.focus();
  }, [paletteOpen]);

  // Keep the cursor row in view when moving by keyboard.
  useEffect(() => {
    listRef.current
      ?.querySelector<HTMLElement>('[data-active="true"]')
      ?.scrollIntoView({ block: "nearest" });
  }, [cursor]);

  if (!paletteOpen) return null;

  const runAt = (i: number) => {
    const cmd = matches[i];
    if (!cmd) return;
    close();
    cmd.run();
  };

  return (
    <div
      className="cmdk-backdrop"
      role="presentation"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) close();
      }}
    >
      <div className="cmdk" role="dialog" aria-modal="true" aria-label="命令面板">
        <div className="cmdk-search">
          <Icon name="search" size={16} className="cmdk-search-icon" />
          <input
            ref={inputRef}
            className="cmdk-input"
            value={query}
            placeholder="输入命令或搜索…"
            autoComplete="off"
            spellCheck={false}
            onChange={(e) => {
              setQuery(e.target.value);
              setCursor(0);
            }}
            onKeyDown={(e) => {
              if (e.key === "Escape") {
                e.preventDefault();
                close();
              } else if (e.key === "ArrowDown" || (e.key === "n" && e.ctrlKey)) {
                e.preventDefault();
                setCursor((c) => (matches.length ? (c + 1) % matches.length : 0));
              } else if (e.key === "ArrowUp" || (e.key === "p" && e.ctrlKey)) {
                e.preventDefault();
                setCursor((c) => (matches.length ? (c - 1 + matches.length) % matches.length : 0));
              } else if (e.key === "Enter") {
                e.preventDefault();
                runAt(cursor);
              }
            }}
          />
        </div>

        <div className="cmdk-list" ref={listRef} role="listbox">
          {matches.length === 0 ? (
            <p className="cmdk-empty">没有匹配的命令</p>
          ) : (
            matches.map((c, i) => (
              <button
                key={c.id}
                role="option"
                aria-selected={i === cursor}
                data-active={i === cursor}
                className={`cmdk-item${i === cursor ? " active" : ""}`}
                onMouseMove={() => setCursor(i)}
                onClick={() => runAt(i)}
              >
                <span className="cmdk-item-icon">
                  <Icon name={c.icon} size={15} />
                </span>
                <span className="cmdk-item-label">{c.label}</span>
                {c.hint && <span className="cmdk-item-hint">{c.hint}</span>}
                {c.shortcut && <kbd className="cmdk-item-key">{c.shortcut}</kbd>}
              </button>
            ))
          )}
        </div>
      </div>
    </div>
  );
}
