/**
 * Custom right-click menu.
 *
 * A packaged Tauri webview ships no context menu at all, so without this there
 * is no copy or paste anywhere in the app — not even in a text field. The menu
 * therefore serves two jobs: clipboard actions wherever text is involved, and
 * message actions when the click landed on a mail row.
 */

import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { Icon } from "./Icon";
import "./ContextMenu.css";

export interface MenuItem {
  id: string;
  label: string;
  icon?: string;
  /** Rendered right-aligned, e.g. a shortcut. */
  hint?: string;
  danger?: boolean;
  disabled?: boolean;
  run: () => void | Promise<void>;
}

/** A rule separating groups. */
export const SEP: MenuItem = { id: "-", label: "-", run: () => {} };

export interface MenuState {
  x: number;
  y: number;
  items: MenuItem[];
}

/** Clipboard actions for whatever the click landed on. */
export function clipboardItems(target: EventTarget | null): MenuItem[] {
  const el = target as HTMLElement | null;
  const field =
    el?.closest?.("input, textarea") as HTMLInputElement | HTMLTextAreaElement | null;
  const selection = window.getSelection()?.toString() ?? "";
  const editable = !!field && !field.readOnly && !field.disabled;

  const items: MenuItem[] = [];

  if (editable) {
    const picked = field.value.slice(field.selectionStart ?? 0, field.selectionEnd ?? 0);
    items.push({
      id: "cut",
      label: "剪切",
      icon: "x",
      hint: "Ctrl+X",
      disabled: !picked,
      run: async () => {
        await navigator.clipboard.writeText(picked);
        const start = field.selectionStart ?? 0;
        const end = field.selectionEnd ?? 0;
        field.setRangeText("", start, end, "end");
        // setRangeText bypasses React's synthetic events, so the controlled
        // value has to be told the DOM changed under it.
        field.dispatchEvent(new Event("input", { bubbles: true }));
      },
    });
  }

  items.push({
    id: "copy",
    label: "复制",
    icon: "copy",
    hint: "Ctrl+C",
    disabled: !selection && !(field && field.selectionStart !== field.selectionEnd),
    run: async () => {
      const text =
        selection ||
        (field ? field.value.slice(field.selectionStart ?? 0, field.selectionEnd ?? 0) : "");
      if (text) await navigator.clipboard.writeText(text);
    },
  });

  if (editable) {
    items.push(
      {
        id: "paste",
        label: "粘贴",
        icon: "edit",
        hint: "Ctrl+V",
        run: async () => {
          const text = await navigator.clipboard.readText();
          if (!text) return;
          const start = field.selectionStart ?? field.value.length;
          const end = field.selectionEnd ?? field.value.length;
          field.setRangeText(text, start, end, "end");
          field.dispatchEvent(new Event("input", { bubbles: true }));
        },
      },
      {
        id: "select-all",
        label: "全选",
        hint: "Ctrl+A",
        run: () => field.select(),
      },
    );
  }

  return items;
}

export function ContextMenu({
  state,
  onClose,
}: {
  state: MenuState | null;
  onClose: () => void;
}) {
  const ref = useRef<HTMLDivElement | null>(null);
  const [pos, setPos] = useState({ x: 0, y: 0 });

  // Flip the menu back inside the window before it paints, so it never opens
  // half off-screen near an edge.
  useLayoutEffect(() => {
    if (!state) return;
    const el = ref.current;
    const w = el?.offsetWidth ?? 200;
    const h = el?.offsetHeight ?? 200;
    const pad = 8;
    setPos({
      x: Math.min(state.x, window.innerWidth - w - pad),
      y: Math.min(state.y, window.innerHeight - h - pad),
    });
  }, [state]);

  useEffect(() => {
    if (!state) return;
    const close = () => onClose();
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    // `capture` so a click that also triggers something else still dismisses.
    window.addEventListener("mousedown", close, true);
    window.addEventListener("resize", close);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("mousedown", close, true);
      window.removeEventListener("resize", close);
      window.removeEventListener("keydown", onKey);
    };
  }, [state, onClose]);

  if (!state || state.items.length === 0) return null;

  return (
    <div
      className="ctxm"
      ref={ref}
      role="menu"
      style={{ left: pos.x, top: pos.y }}
      onMouseDown={(e) => e.stopPropagation()}
    >
      {state.items.map((item, i) =>
        item.id === "-" ? (
          <div className="ctxm-sep" key={`sep-${i}`} role="separator" />
        ) : (
          <button
            key={item.id}
            role="menuitem"
            className={`ctxm-item${item.danger ? " danger" : ""}`}
            disabled={item.disabled}
            onClick={() => {
              onClose();
              void item.run();
            }}
          >
            <span className="ctxm-icon">
              {item.icon && <Icon name={item.icon} size={14} />}
            </span>
            <span className="ctxm-label">{item.label}</span>
            {item.hint && <span className="ctxm-hint">{item.hint}</span>}
          </button>
        ),
      )}
    </div>
  );
}

/**
 * Owns the menu state, and — unless told otherwise — installs the window-wide
 * `contextmenu` handler that supplies the clipboard fallback.
 *
 * Pass `clipboardFallback: false` for a menu that only opens where its owner
 * says so. Two instances both installing the fallback would answer the same
 * right-click twice and stack two identical menus on top of each other.
 */
export function useContextMenu({ clipboardFallback = true } = {}) {
  const [state, setState] = useState<MenuState | null>(null);
  const close = useCallback(() => setState(null), []);

  const openAt = useCallback((e: MouseEvent | React.MouseEvent, items: MenuItem[]) => {
    e.preventDefault();
    if (items.length === 0) return;
    setState({ x: e.clientX, y: e.clientY, items });
  }, []);

  useEffect(() => {
    if (!clipboardFallback) return;
    const onCtx = (e: MouseEvent) => {
      // Rows and other rich targets handle their own menu, and React's handler
      // has already run by the time this window listener sees the event, so a
      // prevented default means somebody richer took it.
      if (e.defaultPrevented) return;
      const items = clipboardItems(e.target);
      if (items.length === 0) {
        e.preventDefault();
        return;
      }
      openAt(e, items);
    };
    window.addEventListener("contextmenu", onCtx);
    return () => window.removeEventListener("contextmenu", onCtx);
  }, [openAt, clipboardFallback]);

  return { state, close, openAt };
}

/**
 * The app-wide clipboard menu.
 *
 * It belongs at the shell rather than inside any one pane: a packaged Tauri
 * webview has no native context menu, so wherever this is not mounted there is
 * no copy or paste at all — which is what the settings and onboarding screens
 * used to be, since the only instance lived in the mail list.
 */
export function GlobalContextMenu() {
  const { state, close } = useContextMenu();
  return <ContextMenu state={state} onClose={close} />;
}
