/**
 * Global keyboard shortcuts, and the cheat sheet that documents them.
 *
 * The rule that keeps this from fighting the rest of the app: a bare letter
 * never fires while the user is typing. Only chorded keys (Ctrl/Cmd) survive a
 * focused field, because "c" inside a compose box must stay the letter c.
 */

import { useEffect } from "react";
import { useApp } from "../lib/store";
import "./CommandPalette.css";

/** True when the event came from somewhere the user is entering text. */
function isTyping(target: EventTarget | null): boolean {
  const el = target as HTMLElement | null;
  if (!el) return false;
  const tag = el.tagName;
  return (
    tag === "INPUT" ||
    tag === "TEXTAREA" ||
    tag === "SELECT" ||
    el.isContentEditable === true
  );
}

const SHEET: Array<{ group: string; rows: Array<[string, string]> }> = [
  {
    group: "全局",
    rows: [
      ["Ctrl / ⌘ + K", "命令面板"],
      ["Ctrl / ⌘ + J", "AI 助手"],
      ["?", "本速查表"],
      ["Esc", "关闭当前弹层"],
    ],
  },
  {
    group: "邮件",
    rows: [
      ["C", "写邮件"],
      ["/", "搜索"],
      ["R", "回复当前邮件"],
      ["S", "加/取消星标"],
      ["#", "删除当前邮件（同时删除服务器）"],
      ["U", "返回列表"],
    ],
  },
  {
    group: "浏览",
    rows: [
      ["J / ↓", "下一封"],
      ["K / ↑", "上一封"],
      ["Enter", "打开选中邮件"],
      ["G 然后 I", "转到全部邮件"],
      ["G 然后 V", "转到验证码"],
      ["G 然后 S", "转到设置"],
    ],
  },
];

/** Installs the global key handler. Renders nothing. */
export function ShortcutListener() {
  const app = useApp();

  useEffect(() => {
    // `g` starts a two-key sequence (g then i, g then v…), the convention every
    // keyboard mail client uses. It lapses after a moment so a stray g does not
    // silently swallow the next keystroke.
    let pendingG = false;
    let gTimer: number | undefined;

    /**
     * Move the selection `delta` rows through the loaded list. With nothing
     * selected, "next" means the first row and "previous" the last, so a single
     * keystroke gets you into the list from either end.
     */
    const step = (delta: number) => {
      const items = app.page.items;
      if (items.length === 0) return;
      const at = app.selectedId ? items.findIndex((m) => m.id === app.selectedId) : -1;
      const next =
        at < 0
          ? delta > 0
            ? 0
            : items.length - 1
          : Math.min(items.length - 1, Math.max(0, at + delta));
      if (next !== at) void app.select(items[next].id);
    };

    const onKey = (e: KeyboardEvent) => {
      const mod = e.ctrlKey || e.metaKey;

      // Chorded shortcuts work anywhere, including inside a text field.
      if (mod && e.key.toLowerCase() === "k") {
        e.preventDefault();
        app.setPaletteOpen(!app.paletteOpen);
        return;
      }
      if (mod && e.key.toLowerCase() === "j") {
        e.preventDefault();
        app.setAssistantOpen(!app.assistantOpen);
        return;
      }

      if (e.key === "Escape") {
        // Innermost layer first, so Escape peels one thing at a time.
        if (app.paletteOpen) app.setPaletteOpen(false);
        else if (app.shortcutsOpen) app.setShortcutsOpen(false);
        else if (app.assistantOpen) app.setAssistantOpen(false);
        return;
      }

      if (mod || e.altKey || isTyping(e.target)) return;
      // A modal owns the keyboard while it is open.
      if (app.paletteOpen || app.shortcutsOpen || app.compose) return;

      if (pendingG) {
        pendingG = false;
        window.clearTimeout(gTimer);
        switch (e.key.toLowerCase()) {
          case "i":
            e.preventDefault();
            app.setFilter({ category: null, starredOnly: false, unreadOnly: false });
            return;
          case "v":
            e.preventDefault();
            app.setFilter({ category: "verification" });
            return;
          case "s":
            e.preventDefault();
            app.openSettings();
            return;
          default:
            return;
        }
      }

      switch (e.key) {
        case "?":
          e.preventDefault();
          app.setShortcutsOpen(true);
          break;
        // The list's own ArrowUp/ArrowDown only reach it while it has focus, so
        // j/k were documented on the cheat sheet and did nothing everywhere
        // else. They step the selection instead of a cursor: from anywhere in
        // the app, the next letter opens the next mail.
        case "j":
        case "J":
          e.preventDefault();
          step(1);
          break;
        case "k":
        case "K":
          e.preventDefault();
          step(-1);
          break;
        case "g":
        case "G":
          pendingG = true;
          gTimer = window.setTimeout(() => {
            pendingG = false;
          }, 1200);
          break;
        case "c":
        case "C":
          e.preventDefault();
          app.openCompose();
          break;
        case "/":
          e.preventDefault();
          document.querySelector<HTMLInputElement>(".ml-search input")?.focus();
          break;
        case "u":
        case "U":
          if (app.selectedId) {
            e.preventDefault();
            void app.select(null);
          }
          break;
        case "s":
        case "S":
          if (app.selectedId) {
            e.preventDefault();
            void app.toggleStar(app.selectedId, !app.selected?.starred);
          }
          break;
        case "r":
        case "R":
          if (app.selected) {
            e.preventDefault();
            app.openCompose({
              accountId: app.selected.accountId,
              to: app.selected.fromAddr,
              subject: app.selected.subject.startsWith("Re:")
                ? app.selected.subject
                : `Re: ${app.selected.subject}`,
              inReplyTo: app.selected.id,
            });
          }
          break;
        case "#":
          if (app.selectedId) {
            e.preventDefault();
            void app.remove([app.selectedId]);
          }
          break;
        default:
          break;
      }
    };

    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("keydown", onKey);
      window.clearTimeout(gTimer);
    };
  }, [app]);

  return null;
}

export function ShortcutSheet() {
  const { shortcutsOpen, setShortcutsOpen } = useApp();
  if (!shortcutsOpen) return null;

  return (
    <div
      className="cmdk-backdrop"
      role="presentation"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) setShortcutsOpen(false);
      }}
    >
      <div className="keys" role="dialog" aria-modal="true" aria-label="键盘快捷键">
        <h2 className="keys-title">键盘快捷键</h2>
        <p className="keys-lede">在输入框中打字时，单字母快捷键不会触发。</p>
        <div className="keys-groups">
          {SHEET.map((g) => (
            <section key={g.group} className="keys-group">
              <h3>{g.group}</h3>
              {g.rows.map(([key, desc]) => (
                <div key={key} className="keys-row">
                  <span className="keys-desc">{desc}</span>
                  <kbd>{key}</kbd>
                </div>
              ))}
            </section>
          ))}
        </div>
      </div>
    </div>
  );
}
