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
  MessagePage,
  SyncStatus,
} from "./types";

export type ThemePref = "light" | "dark" | "system";
export type View = "mail" | "settings";
export type SettingsTab = "accounts" | "ai" | "channels" | "about";

export interface MailFilter {
  accountId: string | null;
  category: Category | null;
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
  starredOnly: false,
  unreadOnly: false,
  search: "",
};

const PAGE_SIZE = 60;

interface AppStore {
  // data
  accounts: AccountPublic[];
  counts: CategoryCount[];
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
  alerts: AlertEvent[];
  toasts: Toast[];
  compose: ComposeState | null;

  // actions
  refreshAccounts: () => Promise<void>;
  refreshList: () => Promise<void>;
  loadMore: () => Promise<void>;
  setFilter: (patch: Partial<MailFilter>) => void;
  select: (id: string | null) => Promise<void>;
  toggleStar: (id: string, starred: boolean) => Promise<void>;
  markRead: (ids: string[], read: boolean) => Promise<void>;
  remove: (ids: string[], onServer: boolean) => Promise<void>;
  sync: (accountId?: string | null) => Promise<void>;
  openSettings: (tab?: SettingsTab) => void;
  closeSettings: () => void;
  setTheme: (t: ThemePref) => void;
  pushToast: (kind: Toast["kind"], text: string) => void;
  dismissToast: (id: number) => void;
  dismissAlert: (messageId: string) => void;
  openAlertMessage: (a: AlertEvent) => Promise<void>;
  openCompose: (init?: Partial<ComposeState>) => void;
  closeCompose: () => void;
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
  const [alerts, setAlerts] = useState<AlertEvent[]>([]);
  const [toasts, setToasts] = useState<Toast[]>([]);
  const [compose, setCompose] = useState<ComposeState | null>(null);

  const filterRef = useRef(filter);
  filterRef.current = filter;
  const pageRef = useRef(page);
  pageRef.current = page;

  // -- theme ----------------------------------------------------------------
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

  const queryFromFilter = useCallback((f: MailFilter, offset: number) => {
    return {
      accountId: f.accountId,
      category: f.category,
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
        if (msg.unread) {
          await api.markRead([id], true);
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

  const remove = useCallback(
    async (ids: string[], onServer: boolean) => {
      try {
        await api.deleteMessages(ids, onServer);
        if (selectedId && ids.includes(selectedId)) {
          setSelected(null);
          setSelectedId(null);
        }
        await refreshList();
        pushToast("ok", onServer ? "已删除（含服务器）" : "已从本地删除");
      } catch (e) {
        pushToast("error", `删除失败: ${e}`);
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
      }, 400);
    };

    void api.onAlert((e) => {
      if (disposed) return;
      setAlerts((a) =>
        a.some((x) => x.messageId === e.messageId) ? a : [...a, e],
      );
    }).then((u) => unsubs.push(u));

    void api.onMailChanged(() => {
      if (!disposed) debouncedRefresh();
    }).then((u) => unsubs.push(u));

    void api.onSyncStatus((s) => {
      if (!disposed) setSyncMap((m) => ({ ...m, [s.accountId]: s }));
    }).then((u) => unsubs.push(u));

    // initial load
    void refreshAccounts();
    void api.syncStatuses().then((list) => {
      if (disposed) return;
      const m: Record<string, SyncStatus> = {};
      for (const s of list) m[s.accountId] = s;
      setSyncMap(m);
    });

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
      alerts,
      toasts,
      compose,
      refreshAccounts,
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
      pushToast,
      dismissToast,
      dismissAlert,
      openAlertMessage,
      openCompose,
      closeCompose,
    }),
    [
      accounts, counts, syncMap, page, filter, selected, selectedId,
      loadingList, loadingMore, view, settingsTab, theme, alerts, toasts, compose,
      refreshAccounts, refreshList, loadMore, setFilter, select, toggleStar,
      markReadAction, remove, sync, openSettings, closeSettings, setTheme,
      pushToast, dismissToast, dismissAlert, openAlertMessage, openCompose, closeCompose,
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
