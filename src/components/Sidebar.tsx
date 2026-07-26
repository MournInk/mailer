/**
 * Left navigation column: wordmark + compose, smart views (categories,
 * starred, unread), the account list with live sync indicators, and a footer
 * with sync / theme / settings.
 *
 * Under 900px `.mail-layout` shrinks this pane to a 64px icon rail; every
 * text label is hidden by CSS there, so each control carries a `title=`.
 */

import { Icon } from "./Icon";
import { useApp, type ThemePref } from "../lib/store";
import { CATEGORY_LABEL, type Category, type SyncStatus } from "../lib/types";
import "./Sidebar.css";

/** Category rows in fixed display order, each with its icon + color token. */
const CATEGORY_ROWS: Array<{ key: Category; icon: string; color: string }> = [
  { key: "verification", icon: "key", color: "var(--cat-verification)" },
  { key: "important", icon: "alert", color: "var(--cat-important)" },
  { key: "normal", icon: "mail", color: "var(--cat-normal)" },
  { key: "spam", icon: "archive", color: "var(--cat-spam)" },
];

/** In-flight sync phases → tooltip text. `idle` / `error` are handled apart. */
const PHASE_LABEL: Record<string, string> = {
  connecting: "正在连接服务器",
  fetching: "正在接收邮件",
  classifying: "AI 正在分析",
};

/** Theme button: system → light → dark → system. */
const THEME_NEXT: Record<ThemePref, ThemePref> = {
  system: "light",
  light: "dark",
  dark: "system",
};
const THEME_META: Record<ThemePref, { icon: string; label: string }> = {
  system: { icon: "monitor", label: "跟随系统" },
  light: { icon: "sun", label: "浅色" },
  dark: { icon: "moon", label: "深色" },
};

export function Sidebar() {
  const {
    accounts,
    counts,
    syncMap,
    filter,
    theme,
    setFilter,
    sync,
    setTheme,
    openSettings,
    openCompose,
  } = useApp();

  const unreadOf = (key: string) =>
    counts.find((c) => c.category === key)?.unread ?? 0;
  const totalUnread = counts.reduce((n, c) => n + c.unread, 0);
  // Mail the classifier has not reached yet — only worth a row when non-empty.
  const pending = counts.find((c) => c.category === "pending");

  // "全部邮件" is the neutral state: no category and no starred/unread toggle.
  const allActive = !filter.category && !filter.starredOnly && !filter.unreadOnly;

  const syncing = accounts.some((a) => {
    const s = syncMap[a.id];
    return !!s && s.phase !== "idle" && s.phase !== "error";
  });

  const nextTheme = THEME_NEXT[theme];

  return (
    <nav className="sidebar-pane">
      <header className="side-head">
        <span className="wordmark">Mailer</span>
        <span className="wordmark-mini" aria-hidden>
          M
        </span>
        <button
          className="icon-btn"
          title="写邮件"
          aria-label="写邮件"
          onClick={() => openCompose()}
        >
          <Icon name="edit" size={16} />
        </button>
      </header>

      <div className="side-body">
        <section className="side-section">
          <div className="side-section-head">视图</div>
          <div className="side-rule" />

          <NavRow
            icon="inbox"
            label="全部邮件"
            count={totalUnread}
            active={allActive}
            onClick={() =>
              setFilter({ category: null, starredOnly: false, unreadOnly: false })
            }
          />

          {CATEGORY_ROWS.map((row) => (
            <NavRow
              key={row.key}
              icon={row.icon}
              color={row.color}
              label={CATEGORY_LABEL[row.key]}
              count={unreadOf(row.key)}
              active={filter.category === row.key}
              onClick={() => setFilter({ category: row.key })}
            />
          ))}

          {pending && pending.total > 0 && (
            <NavRow
              icon="spark"
              label="待分类"
              title={`待分类：${pending.total} 封邮件尚未由 AI 处理`}
              count={pending.total}
              static
            />
          )}

          <NavRow
            icon="star"
            label="星标"
            active={filter.starredOnly}
            onClick={() => setFilter({ starredOnly: !filter.starredOnly })}
          />
          <NavRow
            icon="bell"
            label="未读"
            count={totalUnread}
            active={filter.unreadOnly}
            onClick={() => setFilter({ unreadOnly: !filter.unreadOnly })}
          />
        </section>

        <section className="side-section">
          <div className="side-section-head">账户</div>
          <div className="side-rule" />

          {accounts.map((a) => {
            const active = filter.accountId === a.id;
            return (
              <button
                key={a.id}
                className={`acct-row${active ? " active" : ""}`}
                title={`${a.label} · ${a.email}`}
                aria-current={active ? "true" : undefined}
                // clicking the active account clears the filter again
                onClick={() => setFilter({ accountId: active ? null : a.id })}
              >
                <span
                  className="acct-avatar"
                  style={{ background: `hsl(${a.colorHue} 55% 45%)` }}
                >
                  {a.label.trim().charAt(0) || "?"}
                </span>
                <span className="acct-text">
                  <span className="acct-name">{a.label}</span>
                  <span className="acct-mail">{a.email}</span>
                </span>
                <SyncMark status={syncMap[a.id]} />
              </button>
            );
          })}
        </section>
      </div>

      <footer className="side-foot">
        <button
          className="btn btn-ghost btn-sm sync-btn"
          title="立即同步全部账户"
          onClick={() => void sync()}
        >
          <Icon name="refresh" size={15} className={syncing ? "spin" : undefined} />
          <span className="btn-text">同步</span>
        </button>
        <button
          className="icon-btn"
          title={`主题：${THEME_META[theme].label}（切换为${THEME_META[nextTheme].label}）`}
          aria-label="切换主题"
          onClick={() => setTheme(nextTheme)}
        >
          <Icon name={THEME_META[theme].icon} size={16} />
        </button>
        <button
          className="icon-btn"
          title="设置"
          aria-label="设置"
          onClick={() => openSettings()}
        >
          <Icon name="settings" size={16} />
        </button>
      </footer>
    </nav>
  );
}

/** One smart-view row. `static` renders an informational row with no action. */
function NavRow({
  icon,
  label,
  color,
  count = 0,
  active = false,
  title,
  static: isStatic = false,
  onClick,
}: {
  icon: string;
  label: string;
  color?: string;
  count?: number;
  active?: boolean;
  title?: string;
  static?: boolean;
  onClick?: () => void;
}) {
  return (
    <button
      className={`nav-row${active ? " active" : ""}${isStatic ? " static" : ""}`}
      title={title ?? label}
      aria-current={active ? "true" : undefined}
      // not `disabled` — a disabled button swallows its own tooltip
      aria-disabled={isStatic ? "true" : undefined}
      onClick={isStatic ? undefined : onClick}
    >
      <span className="nav-icon" style={color ? { color } : undefined}>
        <Icon name={icon} size={16} />
      </span>
      <span className="nav-label">{label}</span>
      {count > 0 && (
        <>
          <span className="nav-count">{count > 99 ? "99+" : count}</span>
          {/* rail-only stand-in for the pill */}
          <span className="nav-dot" />
        </>
      )}
    </button>
  );
}

/** Live per-account sync state; idle accounts show nothing. */
function SyncMark({ status }: { status: SyncStatus | undefined }) {
  if (!status || status.phase === "idle") return null;
  if (status.phase === "error") {
    return (
      <span className="acct-sync error" title={status.error ?? "同步失败"}>
        <Icon name="alert" size={14} />
      </span>
    );
  }
  return (
    <span className="acct-sync" title={PHASE_LABEL[status.phase] ?? "同步中"}>
      <Icon name="loader" size={14} className="spin" />
    </span>
  );
}
