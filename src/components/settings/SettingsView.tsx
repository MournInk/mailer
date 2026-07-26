/**
 * Settings shell: a tab rail on the left, a scrollable single-column body on
 * the right. The active tab lives in the store, so `openSettings("ai")` from
 * anywhere in the app lands directly on the right pane.
 *
 * Under 760px the rail folds into a top tab strip — the four labels are short
 * enough to share one row, so nothing ever scrolls sideways.
 */

import { useEffect } from "react";
import { useApp, type SettingsTab } from "../../lib/store";
import { Icon } from "../Icon";
import { OverlayScroll } from "../OverlayScroll";
import { AboutTab } from "./AboutTab";
import { AccountsTab } from "./AccountsTab";
import { AiTab } from "./AiTab";
import { KnowledgeTab } from "./KnowledgeTab";
import { ChannelsTab } from "./ChannelsTab";
import "./Settings.css";

const TABS: Array<{ key: SettingsTab; label: string; icon: string; sub: string }> = [
  {
    key: "accounts",
    label: "账户",
    icon: "mail",
    sub: "管理收发邮件的邮箱账户",
  },
  {
    key: "ai",
    label: "AI 过滤器",
    icon: "spark",
    sub: "配置负责分类、摘要与提取验证码的模型",
  },
  {
    key: "knowledge",
    label: "知识库",
    icon: "archive",
    sub: "语义检索的向量与重排模型，以及助手的记忆",
  },
  {
    key: "channels",
    label: "通知渠道",
    icon: "bell",
    sub: "把重要邮件推送到手机上的外部渠道",
  },
  {
    key: "about",
    label: "关于",
    icon: "shield",
    sub: "版本信息、外观与隐私说明",
  },
];

export function SettingsView() {
  const { settingsTab, compose, openSettings, closeSettings } = useApp();
  const active = TABS.find((t) => t.key === settingsTab) ?? TABS[0];

  // Esc returns to the mail view — but not while the compose modal owns it.
  useEffect(() => {
    if (compose) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        closeSettings();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [compose, closeSettings]);

  return (
    <div className="settings-view">
      <nav className="set-rail" aria-label="设置分类">
        <div className="set-rail-head">设置</div>
        {TABS.map((t) => (
          <button
            key={t.key}
            className={`set-tab${settingsTab === t.key ? " active" : ""}`}
            aria-current={settingsTab === t.key ? "page" : undefined}
            onClick={() => openSettings(t.key)}
          >
            <span className="set-tab-icon">
              <Icon name={t.icon} size={16} />
            </span>
            <span className="set-tab-label">{t.label}</span>
          </button>
        ))}
      </nav>

      <div className="set-main">
        {/* The header shares the body's measure, so the page title, the toolbar
            under it and the cards below all start on one left edge. Left-aligned
            against a centred column, the two read as different pages. */}
        <header className="set-head">
          <div className="set-head-inner">
            <div className="set-head-text">
              <h1 className="set-title">{active.label}</h1>
              <p className="set-subtitle">{active.sub}</p>
            </div>
            <button
              className="btn btn-ghost set-close"
              onClick={closeSettings}
              title="返回邮件（Esc）"
              aria-label="关闭设置"
            >
              <Icon name="back" size={16} />
              <span className="set-close-label">返回邮件</span>
            </button>
          </div>
        </header>

        <OverlayScroll className="set-scroll">
          {/* keyed so switching tabs replays the fade instead of morphing */}
          <div className="set-col fade-up" key={settingsTab}>
            {settingsTab === "accounts" && <AccountsTab />}
            {settingsTab === "ai" && <AiTab />}
            {settingsTab === "knowledge" && <KnowledgeTab />}
            {settingsTab === "channels" && <ChannelsTab />}
            {settingsTab === "about" && <AboutTab />}
          </div>
        </OverlayScroll>
      </div>
    </div>
  );
}
