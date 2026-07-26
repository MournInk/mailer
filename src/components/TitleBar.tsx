/**
 * Custom window chrome for the platforms that let us draw it.
 *
 * The window runs undecorated on Windows and Linux so the title bar belongs to
 * the app rather than the OS — a stock Windows caption bar sits badly against
 * this palette. macOS keeps its traffic lights (Mac users reach for them by
 * muscle memory); we only inset our own header out of their way. Mobile has no
 * window chrome at all, so nothing renders.
 */

import { useEffect, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { hostPlatform } from "../lib/api";
import "./TitleBar.css";

type Chrome = "custom" | "inset" | "none";

/** Cached so a remount does not re-cross the IPC boundary. */
let cachedChrome: Chrome | null = null;

function chromeFor(platform: string): Chrome {
  if (platform === "windows" || platform === "linux") return "custom";
  if (platform === "macos") return "inset";
  return "none";
}

export function useWindowChrome(): Chrome {
  const [chrome, setChrome] = useState<Chrome>(cachedChrome ?? "none");

  useEffect(() => {
    if (cachedChrome !== null) return;
    let alive = true;
    hostPlatform()
      .then((p) => {
        cachedChrome = chromeFor(p);
        if (alive) setChrome(cachedChrome);
      })
      // Without a platform answer, drawing no chrome is the safe default:
      // a missing title bar is recoverable, a duplicated one is not.
      .catch(() => {
        cachedChrome = "none";
      });
    return () => {
      alive = false;
    };
  }, []);

  return chrome;
}

export function TitleBar() {
  const chrome = useWindowChrome();
  const [maximized, setMaximized] = useState(false);

  useEffect(() => {
    if (chrome !== "custom") return;
    const win = getCurrentWindow();
    let unlisten: (() => void) | undefined;

    void win.isMaximized().then(setMaximized).catch(() => {});
    // The window can also be resized by dragging or by a keyboard shortcut, so
    // the button state follows the window rather than our own clicks.
    void win
      .onResized(() => {
        void win.isMaximized().then(setMaximized).catch(() => {});
      })
      .then((u) => {
        unlisten = u;
      })
      .catch(() => {});

    return () => unlisten?.();
  }, [chrome]);

  if (chrome === "none") return null;

  // macOS: an empty strip that keeps content clear of the traffic lights.
  if (chrome === "inset") {
    return <div className="titlebar titlebar-inset" data-tauri-drag-region />;
  }

  const win = getCurrentWindow();

  return (
    // No wordmark here: the sidebar carries the brand directly below it, and two
    // stacked "Mailer" labels read as a mistake rather than as emphasis. This
    // strip is the drag region and the window controls, nothing more.
    <div className="titlebar" data-tauri-drag-region>
      <div className="titlebar-drag" data-tauri-drag-region />

      <div className="titlebar-controls">
        <button
          className="win-btn"
          onClick={() => void win.minimize()}
          aria-label="最小化"
          title="最小化"
        >
          <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden>
            <path d="M0 5h10" stroke="currentColor" strokeWidth="1" />
          </svg>
        </button>

        <button
          className="win-btn"
          onClick={() => void win.toggleMaximize()}
          aria-label={maximized ? "向下还原" : "最大化"}
          title={maximized ? "向下还原" : "最大化"}
        >
          {maximized ? (
            <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden>
              <path
                d="M2.5 2.5V0.5h7v7h-2M0.5 2.5h7v7h-7z"
                fill="none"
                stroke="currentColor"
                strokeWidth="1"
              />
            </svg>
          ) : (
            <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden>
              <rect
                x="0.5"
                y="0.5"
                width="9"
                height="9"
                fill="none"
                stroke="currentColor"
                strokeWidth="1"
              />
            </svg>
          )}
        </button>

        <button
          className="win-btn win-btn-close"
          onClick={() => void win.close()}
          aria-label="关闭"
          title="关闭"
        >
          <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden>
            <path d="M0.5 0.5l9 9M9.5 0.5l-9 9" stroke="currentColor" strokeWidth="1" />
          </svg>
        </button>
      </div>
    </div>
  );
}
