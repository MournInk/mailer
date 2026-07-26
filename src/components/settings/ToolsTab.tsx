/**
 * External tools: the MCP servers the assistant may borrow from.
 *
 * The list doubles as the status board. There is nothing to "test" about an MCP
 * server other than connecting to it and reading its tool list, so one 连接
 * button does both and the result stays on the card — a server that connects but
 * offers nothing useful is a thing the user needs to see.
 *
 * The enabled switch saves on the spot, like the notification channels: cutting
 * a server off is something people do in a hurry.
 */

import { useEffect, useState } from "react";
import * as api from "../../lib/api";
import { useApp } from "../../lib/store";
import {
  MCP_AUTH_LABEL,
  MCP_TRANSPORT_LABEL,
  type McpServerPublic,
  type McpServerStatus,
} from "../../lib/types";
import { Icon } from "../Icon";
import { McpServerForm } from "./McpServerForm";

/** null = list only; { server: null } = add form; { server } = edit form. */
type Editing = { server: McpServerPublic | null } | null;

/** Ready-made servers, so the common case is two clicks and a key. */
const PRESETS: Array<{
  label: string;
  hint: string;
  server: Omit<McpServerPublic, "id" | "hasApiKey">;
}> = [
  {
    label: "Exa · 网页搜索",
    hint: "让助手查邮件之外的东西：报错含义、公司背景、链接现在写着什么。默认工具无需密钥。",
    server: {
      name: "exa",
      transport: "http",
      url: "https://mcp.exa.ai/mcp",
      auth: "api-key-header",
      command: "",
      args: [],
      env: {},
      enabled: true,
    },
  },
  {
    label: "GitHub",
    hint: "仓库、Issue 与 PR。需要一个有对应权限的 Personal Access Token。",
    server: {
      name: "github",
      transport: "http",
      url: "https://api.githubcopilot.com/mcp/",
      auth: "bearer",
      command: "",
      args: [],
      env: {},
      enabled: true,
    },
  },
];

export function ToolsTab() {
  const { pushToast } = useApp();

  const [servers, setServers] = useState<McpServerPublic[]>([]);
  const [loading, setLoading] = useState(true);
  const [editing, setEditing] = useState<Editing>(null);
  const [saving, setSaving] = useState(false);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [confirmId, setConfirmId] = useState<string | null>(null);
  const [status, setStatus] = useState<Record<string, McpServerStatus>>({});
  const [probing, setProbing] = useState(false);

  useEffect(() => {
    void (async () => {
      try {
        setServers(await api.getMcpServers());
      } catch (e) {
        pushToast("error", `读取外部工具失败: ${e}`);
      } finally {
        setLoading(false);
      }
    })();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  /** Connect to everything enabled and keep the report on the cards. */
  const probe = async (reconnect = false) => {
    setProbing(true);
    try {
      const report = reconnect ? await api.reconnectMcp() : await api.mcpStatus();
      setStatus(Object.fromEntries(report.map((s) => [s.id, s])));
      // A disabled server reports neither tools nor an error; it is not a
      // connection and must not be counted as one.
      const live = report.filter((s) => !s.error && s.protocolVersion !== "");
      const tools = live.reduce((n, s) => n + s.tools.length, 0);
      pushToast(
        live.length > 0 ? "ok" : "error",
        live.length > 0
          ? `${live.length} 个服务器已连接，共 ${tools} 个工具可用`
          : "没有服务器连接成功",
      );
    } catch (e) {
      pushToast("error", `连接失败: ${e}`);
    } finally {
      setProbing(false);
    }
  };

  const submit = async (input: Parameters<typeof api.saveMcpServer>[0]) => {
    setSaving(true);
    try {
      setServers(await api.saveMcpServer(input));
      setEditing(null);
      // The saved config is untested until it connects, and the old report
      // describes the old settings.
      setStatus({});
      pushToast("ok", input.id ? "已更新，点击「连接」验证" : "已添加，点击「连接」验证");
    } catch (e) {
      pushToast("error", `保存失败: ${e}`);
    } finally {
      setSaving(false);
    }
  };

  const toggle = async (s: McpServerPublic) => {
    setBusyId(s.id);
    try {
      setServers(await api.saveMcpServer({ ...s, apiKey: null, enabled: !s.enabled }));
    } catch (e) {
      pushToast("error", `保存失败: ${e}`);
    } finally {
      setBusyId(null);
    }
  };

  const remove = async (s: McpServerPublic) => {
    setBusyId(s.id);
    try {
      setServers(await api.deleteMcpServer(s.id));
      setConfirmId(null);
      pushToast("ok", `已移除「${s.name}」`);
    } catch (e) {
      pushToast("error", `删除失败: ${e}`);
    } finally {
      setBusyId(null);
    }
  };

  if (editing) {
    return (
      <McpServerForm
        key={editing.server?.id ?? "new"}
        server={editing.server}
        saving={saving}
        onSubmit={(input) => void submit(input)}
        onCancel={() => setEditing(null)}
      />
    );
  }

  return (
    <>
      <div className="set-toolbar">
        <p className="set-toolbar-text">
          助手默认只能查你自己的邮件。接上 MCP 服务器，它就能在需要时去查网页、
          仓库或别的系统，再拿回来跟邮件一起回答。
        </p>
        <button
          className="btn"
          disabled={probing || servers.every((s) => !s.enabled)}
          onClick={() => void probe(true)}
          title="重新连接所有已启用的服务器"
        >
          <Icon
            name={probing ? "loader" : "refresh"}
            size={15}
            className={probing ? "set-spin" : undefined}
          />
          {probing ? "连接中…" : "全部连接"}
        </button>
        <button
          className="btn btn-primary"
          onClick={() => {
            setConfirmId(null);
            setEditing({ server: null });
          }}
        >
          <Icon name="plus" size={16} />
          添加服务器
        </button>
      </div>

      {loading && (
        <p className="set-loading">
          <Icon name="loader" size={15} className="set-spin" />
          正在读取外部工具…
        </p>
      )}

      {!loading && servers.length === 0 && (
        <div className="card set-empty">
          <span className="set-empty-mark">
            <Icon name="link" size={18} />
          </span>
          <span className="set-empty-title">还没有外部工具</span>
          <p className="set-empty-body">
            MCP（Model Context Protocol）是模型调用外部工具的通用协议。
            从下面挑一个开始，或者手动填写任意服务器地址。
          </p>
          <div className="set-presets">
            {PRESETS.map((p) => (
              <button
                key={p.label}
                type="button"
                className="set-preset"
                onClick={() =>
                  setEditing({
                    server: { ...p.server, id: "", hasApiKey: false },
                  })
                }
              >
                <span className="set-preset-title">{p.label}</span>
                <span className="set-preset-hint">{p.hint}</span>
              </button>
            ))}
          </div>
        </div>
      )}

      <div className="set-cards">
        {servers.map((s) => {
          const busy = busyId === s.id;
          const st = status[s.id];
          // A disabled server gets a row with neither tools nor an error — it was
          // never asked. Only a negotiated protocol version means "connected".
          const live = st && !st.error && st.protocolVersion !== "";
          return (
            <article key={s.id} className="card set-card">
              <div className="set-card-main">
                <span className={`set-kind-mark${s.enabled ? " on" : ""}`}>
                  <Icon name={s.transport === "stdio" ? "terminal" : "link"} size={17} />
                </span>
                <div className="set-card-text">
                  <div className="set-card-title">
                    {s.name}
                    {!s.enabled && <span className="set-muted-tag">已停用</span>}
                    {live && (
                      <span className="set-ok-tag">
                        {st.serverName || "已连接"}
                        {st.serverVersion && ` ${st.serverVersion}`}
                      </span>
                    )}
                  </div>
                  <div className="set-card-mail">
                    {s.transport === "stdio"
                      ? [s.command, ...s.args].join(" ")
                      : s.url}
                  </div>
                  <div className="set-card-cats">
                    <span className="field-hint">
                      {MCP_TRANSPORT_LABEL[s.transport]}
                      {s.transport === "http" && s.auth !== "none" && (
                        <> · {MCP_AUTH_LABEL[s.auth]}{s.hasApiKey ? "（已保存密钥）" : "（缺少密钥）"}</>
                      )}
                    </span>
                  </div>
                </div>
                <button
                  type="button"
                  className={`switch${s.enabled ? " on" : ""}`}
                  role="switch"
                  aria-checked={s.enabled}
                  aria-label={`启用「${s.name}」`}
                  title={s.enabled ? "点击停用" : "点击启用"}
                  disabled={busy}
                  onClick={() => void toggle(s)}
                />
              </div>

              {st?.error && (
                <p className="set-error" role="alert">
                  <Icon name="alert" size={14} />
                  <span>{st.error}</span>
                </p>
              )}

              {live && (
                <div className="set-tools">
                  <div className="set-tools-head">
                    协议 {st.protocolVersion} · {st.tools.length} 个工具
                  </div>
                  {st.tools.map((t) => (
                    <div key={t.name} className="set-tool" title={t.description}>
                      <code className="set-code">{t.remoteName}</code>
                      <span className="set-tool-desc">{t.description}</span>
                    </div>
                  ))}
                </div>
              )}

              <div className="set-card-foot">
                <button
                  className="btn btn-sm"
                  disabled={busy || probing || !s.enabled}
                  title={s.enabled ? "连接并读取工具列表" : "先启用这个服务器"}
                  onClick={() => void probe(true)}
                >
                  <Icon
                    name={probing ? "loader" : "send"}
                    size={14}
                    className={probing ? "set-spin" : undefined}
                  />
                  连接
                </button>
                <button
                  className="btn btn-sm"
                  disabled={busy}
                  onClick={() => {
                    setConfirmId(null);
                    setEditing({ server: s });
                  }}
                >
                  <Icon name="edit" size={14} />
                  编辑
                </button>
                <button
                  className="btn btn-sm btn-danger"
                  disabled={busy}
                  onClick={() => setConfirmId(confirmId === s.id ? null : s.id)}
                >
                  <Icon name="trash" size={14} />
                  移除
                </button>
              </div>

              {confirmId === s.id && (
                <div className="set-confirm" role="alertdialog">
                  <Icon name="alert" size={15} />
                  <span className="set-confirm-text">
                    移除「{s.name}」后助手将不再拥有它提供的工具。
                  </span>
                  <button className="btn btn-sm" onClick={() => setConfirmId(null)}>
                    取消
                  </button>
                  <button
                    className="btn btn-sm btn-danger"
                    disabled={busy}
                    onClick={() => void remove(s)}
                  >
                    确认移除
                  </button>
                </div>
              )}
            </article>
          );
        })}
      </div>

      {!loading && servers.length > 0 && (
        <p className="set-note">
          <Icon name="shield" size={14} />
          <span>
            外部工具会把助手发出的查询交给对应服务器。助手被要求只发送必要的关键词
            或标识，不要把邮件正文、验证码或链接贴进去；密钥只保存在本机。
          </span>
        </p>
      )}
    </>
  );
}
