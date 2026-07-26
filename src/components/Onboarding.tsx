/**
 * First-run view, shown by `App` whenever no account exists yet. It is a quiet
 * landing page rather than a wizard: one serif promise, three lines about what
 * the AI actually does, and the two doors into settings.
 */

import { Icon } from "./Icon";
import { useApp } from "../lib/store";
import "./Onboarding.css";

/** The three payoffs of the classifier, in the order they matter to a user. */
const FEATURES: Array<{ icon: string; title: string; body: string; tone: string }> = [
  {
    icon: "key",
    title: "验证码直接弹窗",
    body: "收到验证码时立即弹出提醒，一键复制，不用再翻邮件。",
    tone: "var(--cat-verification)",
  },
  {
    icon: "archive",
    title: "垃圾邮件静默处理",
    body: "推广与垃圾邮件自动归类，可选择同时从服务器删除。",
    tone: "var(--cat-spam)",
  },
  {
    icon: "bell",
    title: "重要邮件多渠道提醒",
    body: "账单、告警等重要邮件推送到 Telegram、Bark 或自定义 Webhook。",
    tone: "var(--cat-important)",
  },
];

export function Onboarding() {
  const { openSettings } = useApp();

  return (
    <div className="onboarding">
      <div className="onboard-inner fade-up">
        <header className="onboard-head">
          <span className="onboard-mark">
            <Icon name="mail" size={19} />
          </span>
          <span className="onboard-wordmark">Mailer</span>
        </header>

        <h1 className="onboard-title">把收件箱交给 AI 打理</h1>
        <p className="onboard-sub">
          同时收取你的多个邮箱，AI 自动分流验证码、垃圾邮件与重要通知，
          只把真正需要你看的内容留在眼前。
        </p>

        <ul className="onboard-features">
          {FEATURES.map((f) => (
            <li key={f.title} className="onboard-feature">
              <span className="onboard-feature-icon" style={{ color: f.tone }}>
                <Icon name={f.icon} size={17} />
              </span>
              <span className="onboard-feature-text">
                <span className="onboard-feature-title">{f.title}</span>
                <span className="onboard-feature-body">{f.body}</span>
              </span>
            </li>
          ))}
        </ul>

        <div className="onboard-actions">
          <button
            className="btn btn-primary onboard-cta"
            onClick={() => openSettings("accounts")}
          >
            <Icon name="plus" size={16} />
            添加邮箱账户
          </button>
          <button
            className="btn onboard-cta"
            onClick={() => openSettings("ai")}
          >
            <Icon name="spark" size={16} />
            配置 AI 过滤器
          </button>
        </div>

        <p className="onboard-note">
          邮箱密码与 API Key 保存在本机数据库，不会上传到任何服务器；当前尚未加密存储。
        </p>
      </div>
    </div>
  );
}
