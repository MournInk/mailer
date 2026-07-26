/**
 * About + appearance. Short by design: what the app is, what it runs on, how
 * it looks, and the one promise that matters — nothing leaves this device.
 */

import { useApp, type ThemePref } from "../../lib/store";
import { Icon } from "../Icon";
import { Section } from "./parts";

const VERSION = "0.1.0";

const THEMES: Array<{ key: ThemePref; label: string; icon: string }> = [
  { key: "system", label: "跟随系统", icon: "monitor" },
  { key: "light", label: "浅色", icon: "sun" },
  { key: "dark", label: "深色", icon: "moon" },
];

const STACK: Array<{ label: string; value: string }> = [
  { label: "核心", value: "Rust · mailer-core" },
  { label: "外壳", value: "Tauri 2" },
  { label: "界面", value: "React 18 · TypeScript" },
  { label: "存储", value: "SQLite（本机）" },
];

export function AboutTab() {
  const { theme, setTheme } = useApp();

  return (
    <>
      <section className="card set-section set-about">
        <span className="set-about-mark">
          <Icon name="mail" size={21} />
        </span>
        <h2 className="set-about-name">Mailer</h2>
        <p className="set-about-version">版本 {VERSION}</p>
        <p className="set-about-body">
          一个多账户邮件客户端：同时收取你的所有邮箱，交给你自己配置的 AI
          模型分流验证码、垃圾邮件与重要通知，并把真正要紧的那几封实时推送到
          Telegram、QQ、Bark 或任意 Webhook。收件箱应该安静，而不是靠你不停地翻。
        </p>
      </section>

      <Section
        title="外观"
        icon="monitor"
        sub="深色模式跟随系统时会随日出日落自动切换。"
      >
        <div className="set-seg" role="radiogroup" aria-label="主题">
          {THEMES.map((t) => (
            <button
              key={t.key}
              type="button"
              className={`set-seg-btn${theme === t.key ? " active" : ""}`}
              role="radio"
              aria-checked={theme === t.key}
              onClick={() => setTheme(t.key)}
            >
              <Icon name={t.icon} size={15} />
              {t.label}
            </button>
          ))}
        </div>
      </Section>

      <Section title="技术栈" icon="code">
        <dl className="set-kv">
          {STACK.map((row) => (
            <div key={row.label} className="set-kv-row">
              <dt className="set-kv-key">{row.label}</dt>
              <dd className="set-kv-value">{row.value}</dd>
            </div>
          ))}
        </dl>
      </Section>

      <Section title="隐私" icon="shield">
        <p className="set-note">
          <Icon name="shield" size={15} />
          <span>
            邮件正文、邮箱密码与 API Key 全部保存在本机的 SQLite
            数据库中，不会上传到任何服务器。应用只会连接你自己填写的邮件服务器、
            模型接口与通知渠道地址。
          </span>
        </p>
      </Section>

      <p className="set-colophon">
        Mailer · 本地优先的邮件客户端
        <br />
        邮件、密钥与配置都留在这台设备上。
      </p>
    </>
  );
}
