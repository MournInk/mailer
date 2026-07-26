/**
 * Global app state (React context). Components read state and call actions
 * from here; all backend IPC flows through `api.ts`.
 */

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import * as api from "./api";
import type {
  AccountPublic,
  AlertEvent,
  Category,
  CategoryCount,
  EmailMessage,
  LabelCount,
  MailLabel,
  MessagePage,
  SyncStatus,
} from "./types";

export type ThemePref = "light" | "dark" | "system";
export type View = "mail" | "settings";
export type SettingsTab =
  | "accounts"
  | "ai"
  | "knowledge"
  | "tools"
  | "privacy"
  | "channels"
  | "about";

export interface MailFilter {
  accountId: string | null;
  category: Category | null;
  /** one of the user's own labels */
  labelId: string | null;
  starredOnly: boolean;
  unreadOnly: boolean;
  search: string;
}

export interface Toast {
  id: number;
  kind: "info" | "ok" | "error";
  text: string;
}

export interface ComposeState {
  accountId: string;
  to: string;
  subject: string;
  body: string;
  inReplyTo: string | null;
}

const EMPTY_FILTER: MailFilter = {
  accountId: null,
  category: null,
  labelId: null,
  starredOnly: false,
  unreadOnly: false,
  search: "",
};

const PAGE_SIZE = 60;

interface AppStore {
  // data
  accounts: AccountPublic[];
  counts: CategoryCount[];
  labels: MailLabel[];
  labelCounts: LabelCount[];
  syncMap: Record<string, SyncStatus>;
  page: MessagePage;
  filter: MailFilter;
  selected: EmailMessage | null;
  selectedId: string | null;
  loadingList: boolean;
  loadingMore: boolean;

  // chrome
  view: View;
  settingsTab: SettingsTab;
  theme: ThemePref;
  /** Refuse remote content in mail until asked, per message. Default on. */
  blockTrackers: boolean;
  /** Show a reply chain as one row. Default on. */
  groupThreads: boolean;
  alerts: AlertEvent[];
  toasts: Toast[];
  compose: ComposeState | null;
  /** The assistant panel, docked beside the reading pane. */
  assistantOpen: boolean;
  /** Superhuman-style command palette. */
  paletteOpen: boolean;
  /** The keyboard-shortcut cheat sheet, opened with "?". */
  shortcutsOpen: boolean;

  // actions
  refreshAccounts: () => Promise<void>;
  /** Re-read the labels and their counts — after editing them, or after a sync. */
  refreshLabels: () => Promise<void>;
  refreshList: () => Promise<void>;
  loadMore: () => Promise<void>;
  setFilter: (patch: Partial<MailFilter>) => void;
  select: (id: string | null) => Promise<void>;
  toggleStar: (id: string, starred: boolean) => Promise<void>;
  markRead: (ids: string[], read: boolean) => Promise<void>;
  /**
   * Delete mail. Defaults to deleting on the server too — in this app "删除"
   * means the message is gone, not merely hidden here. Optimistic: the rows go
   * immediately and come back if the server refuses.
   */
  remove: (ids: string[], onServer?: boolean) => Promise<void>;
  sync: (accountId?: string | null) => Promise<void>;
  openSettings: (tab?: SettingsTab) => void;
  closeSettings: () => void;
  setTheme: (t: ThemePref) => void;
  setBlockTrackers: (v: boolean) => Promise<void>;
  setGroupThreads: (v: boolean) => Promise<void>;
  pushToast: (kind: Toast["kind"], text: string) => void;
  dismissToast: (id: number) => void;
  dismissAlert: (messageId: string) => void;
  openAlertMessage: (a: AlertEvent) => Promise<void>;
  openCompose: (init?: Partial<ComposeState>) => void;
  closeCompose: () => void;
  setAssistantOpen: (open: boolean) => void;
  setPaletteOpen: (open: boolean) => void;
  setShortcutsOpen: (open: boolean) => void;
}

const Ctx = createContext<AppStore | null>(null);

export function useApp(): AppStore {
  const v = useContext(Ctx);
  if (!v) throw new Error("useApp outside provider");
  return v;
}

function applyTheme(pref: ThemePref) {
  const dark =
    pref === "dark" ||
    (pref === "system" && window.matchMedia("(prefers-color-scheme: dark)").matches);
  document.documentElement.dataset.theme = dark ? "dark" : "light";
}

let toastSeq = 1;

export function AppProvider({ children }: { children: ReactNode }) {
  const [accounts, setAccounts] = useState<AccountPublic[]>([]);
  const [counts, setCounts] = useState<CategoryCount[]>([]);
  const [labels, setLabels] = useState<MailLabel[]>([]);
  const [labelCounts, setLabelCounts] = useState<LabelCount[]>([]);
  const [syncMap, setSyncMap] = useState<Record<string, SyncStatus>>({});
  const [page, setPage] = useState<MessagePage>({ items: [], total: 0, unread: 0 });
  const [filter, setFilterState] = useState<MailFilter>(EMPTY_FILTER);
  const [selected, setSelected] = useState<EmailMessage | null>(null);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [loadingList, setLoadingList] = useState(false);
  const [loadingMore, setLoadingMore] = useState(false);
  const [view, setView] = useState<View>("mail");
  const [settingsTab, setSettingsTab] = useState<SettingsTab>("accounts");
  const [theme, setThemeState] = useState<ThemePref>(
    () => (localStorage.getItem("mailer.theme") as ThemePref) || "system",
  );
  // The tracker switch lives here rather than in the settings screen: the
  // reading pane has to honour it the moment it changes, and both need one
  // answer to "are we blocking".
  const [blockTrackers, setBlockTrackersState] = useState(true);
  // Same reasoning for grouping: the list renders a row differently depending
  // on it, and the backend decides what a page contains from the same stored
  // value — so this is a mirror of the setting, never a second opinion.
  const [groupThreads, setGroupThreadsState] = useState(true);
  const [alerts, setAlerts] = useState<AlertEvent[]>([]);
  const [toasts, setToasts] = useState<Toast[]>([]);
  const [compose, setCompose] = useState<ComposeState | null>(null);
  const [assistantOpen, setAssistantOpen] = useState(false);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [shortcutsOpen, setShortcutsOpen] = useState(false);

  const filterRef = useRef(filter);
  filterRef.current = filter;
  const pageRef = useRef(page);
  pageRef.current = page;
  // `select` is memoised on very little, and re-creating it whenever the
  // grouping preference changes would re-run every effect that depends on it.
  const groupThreadsRef = useRef(groupThreads);
  groupThreadsRef.current = groupThreads;

  // -- theme ----------------------------------------------------------------
  // Read the stored preference once. Blocking stays on until told otherwise,
  // which is also what happens if this read fails.
  useEffect(() => {
    void api
      .getPrivacySettings()
      .then((p) => setBlockTrackersState(p.blockTrackers))
      .catch(() => {});
    void api
      .getReadingSettings()
      .then((r) => setGroupThreadsState(r.groupThreads))
      .catch(() => {});
  }, []);

  useEffect(() => {
    applyTheme(theme);
    localStorage.setItem("mailer.theme", theme);
    if (theme !== "system") return;
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    const fn = () => applyTheme("system");
    mq.addEventListener("change", fn);
    return () => mq.removeEventListener("change", fn);
  }, [theme]);

  // -- toasts ---------------------------------------------------------------
  const pushToast = useCallback((kind: Toast["kind"], text: string) => {
    const id = toastSeq++;
    setToasts((t) => [...t, { id, kind, text }]);
    window.setTimeout(() => {
      setToasts((t) => t.filter((x) => x.id !== id));
    }, kind === "error" ? 6000 : 3500);
  }, []);

  const dismissToast = useCallback((id: number) => {
    setToasts((t) => t.filter((x) => x.id !== id));
  }, []);

  // -- data loading ---------------------------------------------------------
  const refreshAccounts = useCallback(async () => {
    try {
      setAccounts(await api.listAccounts());
      setCounts(await api.categoryCounts());
    } catch (e) {
      pushToast("error", `加载账户失败: ${e}`);
    }
  }, [pushToast]);

  const refreshLabels = useCallback(async () => {
    // Quietly: labels are an optional feature and a failure here must not put a
    // toast in front of somebody who never made one.
    try {
      const [ls, cs] = await Promise.all([api.listLabels(), api.labelCounts()]);
      setLabels(ls);
      setLabelCounts(cs);
    } catch {
      /* leave what we have */
    }
  }, []);

  const queryFromFilter = useCallback((f: MailFilter, offset: number) => {
    return {
      accountId: f.accountId,
      category: f.category,
      labelId: f.labelId,
      unreadOnly: f.unreadOnly,
      starredOnly: f.starredOnly,
      search: f.search || null,
      limit: PAGE_SIZE,
      offset,
    };
  }, []);

  const refreshList = useCallback(async () => {
    setLoadingList(true);
    try {
      const f = filterRef.current;
      const p = await api.listMessages(queryFromFilter(f, 0));
      setPage(p);
      setCounts(await api.categoryCounts());
    } catch (e) {
      pushToast("error", `加载邮件失败: ${e}`);
    } finally {
      setLoadingList(false);
    }
  }, [pushToast, queryFromFilter]);

  const loadMore = useCallback(async () => {
    const cur = pageRef.current;
    if (cur.items.length >= cur.total) return;
    setLoadingMore(true);
    try {
      const more = await api.listMessages(
        queryFromFilter(filterRef.current, cur.items.length),
      );
      setPage({
        items: [...cur.items, ...more.items],
        total: more.total,
        unread: more.unread,
      });
    } catch (e) {
      pushToast("error", `加载更多失败: ${e}`);
    } finally {
      setLoadingMore(false);
    }
  }, [pushToast, queryFromFilter]);

  const setFilter = useCallback((patch: Partial<MailFilter>) => {
    setFilterState((f) => ({ ...f, ...patch }));
  }, []);

  // reload the list whenever the filter changes
  useEffect(() => {
    void refreshList();
  }, [filter, refreshList]);

  // -- selection ------------------------------------------------------------
  const select = useCallback(
    async (id: string | null) => {
      setSelectedId(id);
      if (!id) {
        setSelected(null);
        return;
      }
      try {
        const msg = await api.getMessage(id);
        setSelected(msg);
        // Grouped, the row that was clicked stands for the whole conversation,
        // so opening it has to clear the whole conversation. Marking only this
        // message would leave the row bold on an unread reply the user cannot
        // even see from the list.
        const row = pageRef.current.items.find((m) => m.id === id);
        const wholeThread = groupThreadsRef.current && !!msg.threadId;
        if (msg.unread || (wholeThread && row?.unread)) {
          if (wholeThread) {
            await api.markThreadRead(msg.threadId, true);
          } else {
            await api.markRead([id], true);
          }
          setPage((p) => ({
            ...p,
            unread: Math.max(0, p.unread - 1),
            items: p.items.map((m) => (m.id === id ? { ...m, unread: false } : m)),
          }));
          setCounts(await api.categoryCounts());
        }
      } catch (e) {
        pushToast("error", `读取邮件失败: ${e}`);
      }
    },
    [pushToast],
  );

  // -- message actions ------------------------------------------------------
  const toggleStar = useCallback(
    async (id: string, starred: boolean) => {
      try {
        await api.setStarred(id, starred);
        setPage((p) => ({
          ...p,
          items: p.items.map((m) => (m.id === id ? { ...m, starred } : m)),
        }));
        setSelected((s) => (s && s.id === id ? { ...s, starred } : s));
      } catch (e) {
        pushToast("error", `操作失败: ${e}`);
      }
    },
    [pushToast],
  );

  const markReadAction = useCallback(
    async (ids: string[], read: boolean) => {
      try {
        await api.markRead(ids, read);
        await refreshList();
      } catch (e) {
        pushToast("error", `操作失败: ${e}`);
      }
    },
    [pushToast, refreshList],
  );

  /**
   * Delete mail, optimistically.
   *
   * Deleting on the server is a network round trip; waiting for it before the
   * row disappears makes the app feel broken on a slow mailbox. So the rows go
   * first and the request follows. If the server refused, the backend reports
   * which ids it kept and they come back with a warning — a delete that silently
   * failed would otherwise reappear at the next sync with no explanation.
   */
  const remove = useCallback(
    async (ids: string[], onServer = true) => {
      if (ids.length === 0) return;
      const gone = new Set(ids);

      // Hide them now. Computed from the current page rather than counted inside
      // the updater: React invokes updaters twice under StrictMode, and a
      // counter incremented in there would decrement the totals twice.
      const cur = pageRef.current;
      const dropped = cur.items.filter((m) => gone.has(m.id));
      setPage({
        items: cur.items.filter((m) => !gone.has(m.id)),
        total: Math.max(0, cur.total - dropped.length),
        unread: Math.max(0, cur.unread - dropped.filter((m) => m.unread).length),
      });
      const wasOpen = selectedId !== null && gone.has(selectedId);
      if (wasOpen) {
        setSelected(null);
        setSelectedId(null);
      }

      try {
        const report = await api.deleteMessages(ids, onServer);
        if (report.failed.length > 0) {
          // The mail is still on the server, so it has to be visible again.
          await refreshList();
          pushToast(
            "error",
            `${report.failed.length} 封邮件删除失败，已恢复显示：${report.error ?? "服务器未说明原因"}`,
          );
          return;
        }
        // The counters were adjusted from the loaded page only; a full refresh
        // reconciles them with the rest of the mailbox and pulls in the next row.
        void refreshList();
      } catch (e) {
        await refreshList();
        pushToast("error", `删除失败，已恢复显示: ${e}`);
      }
    },
    [pushToast, refreshList, selectedId],
  );

  const sync = useCallback(
    async (accountId?: string | null) => {
      try {
        await api.syncNow(accountId ?? null);
      } catch (e) {
        pushToast("error", `同步失败: ${e}`);
      }
    },
    [pushToast],
  );

  // -- chrome ---------------------------------------------------------------
  const openSettings = useCallback((tab?: SettingsTab) => {
    if (tab) setSettingsTab(tab);
    setView("settings");
  }, []);
  const closeSettings = useCallback(() => setView("mail"), []);

  const setTheme = useCallback((t: ThemePref) => setThemeState(t), []);

  const setBlockTrackers = useCallback(async (v: boolean) => {
    // Optimistic: the pane should stop blocking the instant the switch moves,
    // and a failed write is worth a toast rather than a frozen switch.
    setBlockTrackersState(v);
    try {
      await api.setPrivacySettings({ blockTrackers: v });
    } catch (e) {
      setBlockTrackersState(!v);
      pushToast("error", `保存失败: ${e}`);
    }
  }, [pushToast]);

  const setGroupThreads = useCallback(
    async (v: boolean) => {
      setGroupThreadsState(v);
      try {
        await api.setReadingSettings({ groupThreads: v });
        // The backend reads the stored value to build a page, so the list has
        // to be asked again — nothing about the current one is still true.
        await refreshList();
      } catch (e) {
        setGroupThreadsState(!v);
        pushToast("error", `保存失败: ${e}`);
      }
    },
    [pushToast, refreshList],
  );

  const dismissAlert = useCallback((messageId: string) => {
    setAlerts((a) => a.filter((x) => x.messageId !== messageId));
  }, []);

  const openAlertMessage = useCallback(
    async (a: AlertEvent) => {
      dismissAlert(a.messageId);
      setView("mail");
      await select(a.messageId);
    },
    [dismissAlert, select],
  );

  const openCompose = useCallback(
    (init?: Partial<ComposeState>) => {
      const first = accounts.find((a) => a.hasSmtp) ?? accounts[0];
      if (!first) {
        pushToast("info", "请先添加邮箱账户");
        return;
      }
      setCompose({
        accountId: init?.accountId ?? first.id,
        to: init?.to ?? "",
        subject: init?.subject ?? "",
        body: init?.body ?? "",
        inReplyTo: init?.inReplyTo ?? null,
      });
    },
    [accounts, pushToast],
  );
  const closeCompose = useCallback(() => setCompose(null), []);

  // -- backend events -------------------------------------------------------
  useEffect(() => {
    let disposed = false;
    const unsubs: Array<() => void> = [];
    let refreshTimer: number | undefined;

    const debouncedRefresh = () => {
      window.clearTimeout(refreshTimer);
      refreshTimer = window.setTimeout(() => {
        void refreshList();
        // New mail changes the label counts too, and the sidebar is showing them.
        void refreshLabels();
      }, 400);
    };

    // `listen` throws synchronously — not as a rejected promise — when the
    // Tauri IPC bridge is missing. Unguarded, that throw escapes the effect,
    // React unmounts the tree, and the user gets an empty window with no clue
    // why. Losing live updates is bad; losing the entire UI is worse.
    const subscribe = <T,>(
      what: string,
      register: (cb: (payload: T) => void) => Promise<() => void>,
      handler: (payload: T) => void,
    ) => {
      try {
        void register((payload) => {
          if (!disposed) handler(payload);
        })
          .then((u) => unsubs.push(u))
          .catch((e) => {
            console.error(`failed to subscribe to ${what}:`, e);
          });
      } catch (e) {
        console.error(`failed to subscribe to ${what}:`, e);
      }
    };

    subscribe("alerts", api.onAlert, (e) =>
      setAlerts((a) => (a.some((x) => x.messageId === e.messageId) ? a : [...a, e])),
    );
    subscribe("mail-changed", api.onMailChanged, debouncedRefresh);
    subscribe("sync-status", api.onSyncStatus, (s) =>
      setSyncMap((m) => ({ ...m, [s.accountId]: s })),
    );

    // initial load
    void refreshAccounts();
    void refreshLabels();
    void api
      .syncStatuses()
      .then((list) => {
        if (disposed) return;
        const m: Record<string, SyncStatus> = {};
        for (const s of list) m[s.accountId] = s;
        setSyncMap(m);
      })
      .catch((e) => console.error("failed to read sync statuses:", e));

    return () => {
      disposed = true;
      window.clearTimeout(refreshTimer);
      unsubs.forEach((u) => u());
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const value = useMemo<AppStore>(
    () => ({
      accounts,
      counts,
      labels,
      labelCounts,
      syncMap,
      page,
      filter,
      selected,
      selectedId,
      loadingList,
      loadingMore,
      view,
      settingsTab,
      theme,
      blockTrackers,
      groupThreads,
      alerts,
      toasts,
      compose,
      refreshAccounts,
      refreshLabels,
      refreshList,
      loadMore,
      setFilter,
      select,
      toggleStar,
      markRead: markReadAction,
      remove,
      sync,
      openSettings,
      closeSettings,
      setTheme,
      setBlockTrackers,
      setGroupThreads,
      pushToast,
      dismissToast,
      dismissAlert,
      openAlertMessage,
      openCompose,
      closeCompose,
      assistantOpen,
      paletteOpen,
      shortcutsOpen,
      setAssistantOpen,
      setPaletteOpen,
      setShortcutsOpen,
    }),
    [
      accounts, counts, syncMap, page, filter, selected, selectedId,
      loadingList, loadingMore, view, settingsTab, theme, blockTrackers, groupThreads, alerts, toasts, compose,
      labels, labelCounts, refreshLabels,
      refreshAccounts, refreshList, loadMore, setFilter, select, toggleStar,
      markReadAction, remove, sync, openSettings, closeSettings, setTheme, setGroupThreads,
      pushToast, dismissToast, dismissAlert, openAlertMessage, openCompose, closeCompose,
      assistantOpen, paletteOpen, shortcutsOpen,
    ],
  );

  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}

/** Shared date formatting for list rows and the reading pane. */
export function formatDate(ms: number): string {
  const d = new Date(ms);
  const now = new Date();
  const sameDay = d.toDateString() === now.toDateString();
  if (sameDay) {
    return d.toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" });
  }
  const sameYear = d.getFullYear() === now.getFullYear();
  if (sameYear) {
    return d.toLocaleDateString("zh-CN", { month: "numeric", day: "numeric" });
  }
  return d.toLocaleDateString("zh-CN", { year: "numeric", month: "numeric", day: "numeric" });
}

export function formatFullDate(ms: number): string {
  return new Date(ms).toLocaleString("zh-CN", {
    year: "numeric",
    month: "long",
    day: "numeric",
    weekday: "short",
    hour: "2-digit",
    minute: "2-digit",
  });
}
