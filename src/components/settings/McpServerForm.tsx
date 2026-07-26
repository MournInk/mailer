/**
 * Editor for one MCP server. Which fields show depends on the transport: an HTTP
 * server needs a URL and maybe a key, a local one needs a command and arguments.
 *
 * Values typed for one transport survive switching to the other, so comparing
 * "run it locally" against "use the hosted endpoint" never loses input.
 */

import { useState } from "react";
import {
  MCP_AUTH_LABEL,
  MCP_TRANSPORT_LABEL,
  type McpAuth,
  type McpServerInput,
  type McpServerPublic,
  type McpTransport,
} from "../../lib/types";
import { Icon } from "../Icon";
import { Group } from "./parts";

const TRANSPORTS: McpTransport[] = ["http", "stdio"];
const AUTHS: McpAuth[] = ["none", "bearer", "api-key-header"];
const HTTP_URL = /^https?:\/\/\S+$/i;
/** The characters a tool name may contain, which is what the name becomes. */
const USABLE_NAME = /[a-zA-Z0-9]/;

interface EnvRow {
  key: string;
  value: string;
}

interface Draft {
  id: string;
  name: string;
  transport: McpTransport;
  url: string;
  auth: McpAuth;
  /** "" means "keep whatever is stored". */
  apiKey: string;
  command: string;
  /** One per line, so pasting a command from a README works. */
  args: string;
  env: EnvRow[];
  enabled: boolean;
}

function draftFrom(server: McpServerPublic | null): Draft {
  if (!server) {
    return {
      id: "",
      name: "",
      transport: "http",
      url: "",
      auth: "none",
      apiKey: "",
      command: "",
      args: "",
      env: [],
      enabled: true,
    };
  }
  return {
    id: server.id,
    name: server.name,
    transport: server.transport,
    url: server.url,
    auth: server.auth,
    apiKey: "",
    command: server.command,
    args: server.args.join("\n"),
    env: Object.entries(server.env).map(([key, value]) => ({ key, value })),
    enabled: server.enabled,
  };
}

export function McpServerForm({
  server,
  saving,
  onSubmit,
  onCancel,
}: {
  server: McpServerPublic | null;
  saving: boolean;
  onSubmit: (input: McpServerInput) => void;
  onCancel: () => void;
}) {
  const [draft, setDraft] = useState<Draft>(() => draftFrom(server));
  const [error, setError] = useState<string | null>(null);
  const isEdit = Boolean(server?.id);
  const patch = (p: Partial<Draft>) => setDraft((d) => ({ ...d, ...p }));

  const submit = (e: React.FormEvent) => {
    e.preventDefault();
    const name = draft.name.trim();
    if (!name) return setError("请给这个服务器起一个名字。");
    if (!USABLE_NAME.test(name)) {
      return setError(
        "名字要包含字母或数字：它会成为工具名的一部分（mcp__名字__工具），中文无法出现在工具名里。",
      );
    }
    if (draft.transport === "http") {
      if (!HTTP_URL.test(draft.url.trim())) {
        return setError("请填写完整的 http:// 或 https:// 地址。");
      }
      // On edit an empty key means "keep the stored one", so only a new server
      // with no key at all is a mistake worth blocking.
      if (draft.auth !== "none" && !draft.apiKey.trim() && !server?.hasApiKey) {
        return setError("选择了鉴权方式，请填写密钥，或把鉴权方式改成「无需鉴权」。");
      }
    } else if (!draft.command.trim()) {
      return setError("请填写要运行的命令，例如 npx。");
    }

    setError(null);
    onSubmit({
      id: draft.id || null,
      name,
      transport: draft.transport,
      url: draft.url.trim(),
      auth: draft.auth,
      apiKey: draft.apiKey.trim() || null,
      command: draft.command.trim(),
      args: draft.args
        .split("\n")
        .map((a) => a.trim())
        .filter(Boolean),
      env: Object.fromEntries(
        draft.env.filter((r) => r.key.trim()).map((r) => [r.key.trim(), r.value]),
      ),
      enabled: draft.enabled,
    });
  };

  return (
    <form className="card set-section set-form" onSubmit={submit}>
      <header className="set-section-head">
        <span className="set-section-mark">
          <Icon name="link" size={15} />
        </span>
        <div className="set-section-text">
          <h2 className="set-section-title">
            {isEdit ? "编辑外部工具" : "添加 MCP 服务器"}
          </h2>
          <p className="set-section-sub">
            助手会在需要时调用这里的工具，并在回答里说明信息来自外部。
          </p>
        </div>
        <button
          type="button"
          className="icon-btn"
          onClick={onCancel}
          disabled={saving}
          title="取消"
          aria-label="取消"
        >
          <Icon name="x" size={16} />
        </button>
      </header>

      <div className="set-section-body">
        <div className="field">
          <span className="field-label">连接方式</span>
          <div className="set-choices">
            {TRANSPORTS.map((t) => (
              <button
                key={t}
                type="button"
                className={`set-choice${draft.transport === t ? " active" : ""}`}
                aria-pressed={draft.transport === t}
                disabled={saving}
                onClick={() => patch({ transport: t })}
              >
                <span className="set-choice-icon">
                  <Icon name={t === "stdio" ? "terminal" : "link"} size={15} />
                </span>
                <span className="set-choice-label">{MCP_TRANSPORT_LABEL[t]}</span>
              </button>
            ))}
          </div>
        </div>

        <div className="field">
          <label className="field-label" htmlFor="mcp-name">
            名字
          </label>
          <input
            id="mcp-name"
            className="input"
            value={draft.name}
            disabled={saving}
            autoComplete="off"
            spellCheck={false}
            placeholder="exa"
            onChange={(e) => patch({ name: e.target.value })}
          />
          <p className="field-hint">
            也是工具名的命名空间：叫 <code className="set-code">exa</code> 时，它的
            搜索工具在助手那边就是{" "}
            <code className="set-code">mcp__exa__web_search_exa</code>。用字母和数字。
          </p>
        </div>

        {draft.transport === "http" ? (
          <Group title="远程地址" hint="MCP 的 Streamable HTTP 端点，通常以 /mcp 结尾。">
            <div className="field">
              <label className="field-label" htmlFor="mcp-url">
                地址
              </label>
              <input
                id="mcp-url"
                className="input set-mono"
                value={draft.url}
                disabled={saving}
                autoComplete="off"
                spellCheck={false}
                placeholder="https://mcp.exa.ai/mcp"
                onChange={(e) => patch({ url: e.target.value })}
              />
            </div>

            <div className="field">
              <label className="field-label" htmlFor="mcp-auth">
                鉴权方式
              </label>
              <select
                id="mcp-auth"
                className="input"
                value={draft.auth}
                disabled={saving}
                onChange={(e) => patch({ auth: e.target.value as McpAuth })}
              >
                {AUTHS.map((a) => (
                  <option key={a} value={a}>
                    {MCP_AUTH_LABEL[a]}
                  </option>
                ))}
              </select>
              <p className="field-hint">
                这个没有统一标准：GitHub 用 Bearer，Exa 用 x-api-key。按服务商文档选。
              </p>
            </div>

            {draft.auth !== "none" && (
              <div className="field">
                <label className="field-label" htmlFor="mcp-key">
                  密钥
                </label>
                <input
                  id="mcp-key"
                  className="input set-mono"
                  type="password"
                  value={draft.apiKey}
                  disabled={saving}
                  autoComplete="off"
                  spellCheck={false}
                  placeholder={server?.hasApiKey ? "已保存，留空则不修改" : ""}
                  onChange={(e) => patch({ apiKey: e.target.value })}
                />
                <p className="field-hint">只保存在本机，只会发给上面这个地址。</p>
              </div>
            )}
          </Group>
        ) : (
          <Group
            title="本地进程"
            hint="启动一个在自己的 stdin/stdout 上说 MCP 的程序。它随第一次调用启动，之后一直复用。"
          >
            <div className="field">
              <label className="field-label" htmlFor="mcp-cmd">
                命令
              </label>
              <input
                id="mcp-cmd"
                className="input set-mono"
                value={draft.command}
                disabled={saving}
                autoComplete="off"
                spellCheck={false}
                placeholder="npx"
                onChange={(e) => patch({ command: e.target.value })}
              />
            </div>

            <div className="field">
              <label className="field-label" htmlFor="mcp-args">
                参数（每行一个）
              </label>
              <textarea
                id="mcp-args"
                className="textarea set-mono"
                value={draft.args}
                disabled={saving}
                spellCheck={false}
                placeholder={"-y\n@modelcontextprotocol/server-filesystem\n/Users/me/notes"}
                onChange={(e) => patch({ args: e.target.value })}
              />
              <p className="field-hint">
                一行一个参数，不要写成一整行——带空格的路径才不会被切开。
              </p>
            </div>

            <div className="field">
              <span className="field-label">环境变量（可选）</span>
              {draft.env.map((row, i) => (
                <div key={i} className="set-hdr-row">
                  <input
                    className="input set-mono"
                    value={row.key}
                    disabled={saving}
                    autoComplete="off"
                    spellCheck={false}
                    placeholder="GITHUB_TOKEN"
                    aria-label="变量名"
                    onChange={(e) =>
                      patch({
                        env: draft.env.map((r, j) =>
                          j === i ? { ...r, key: e.target.value } : r,
                        ),
                      })
                    }
                  />
                  <input
                    className="input set-mono"
                    value={row.value}
                    disabled={saving}
                    autoComplete="off"
                    spellCheck={false}
                    aria-label="变量值"
                    onChange={(e) =>
                      patch({
                        env: draft.env.map((r, j) =>
                          j === i ? { ...r, value: e.target.value } : r,
                        ),
                      })
                    }
                  />
                  <button
                    type="button"
                    className="icon-btn"
                    disabled={saving}
                    title="删除该变量"
                    aria-label="删除该变量"
                    onClick={() => patch({ env: draft.env.filter((_, j) => j !== i) })}
                  >
                    <Icon name="x" size={15} />
                  </button>
                </div>
              ))}
              <button
                type="button"
                className="btn btn-sm set-add-hdr"
                disabled={saving}
                onClick={() => patch({ env: [...draft.env, { key: "", value: "" }] })}
              >
                <Icon name="plus" size={14} />
                添加变量
              </button>
            </div>
          </Group>
        )}

        {error && (
          <p className="set-error" role="alert">
            <Icon name="alert" size={14} />
            <span>{error}</span>
          </p>
        )}
      </div>

      <footer className="set-form-foot">
        <span className="set-foot-hint">保存后在列表里点「连接」验证并查看工具。</span>
        <button type="button" className="btn" onClick={onCancel} disabled={saving}>
          取消
        </button>
        <button type="submit" className="btn btn-primary" disabled={saving}>
          <Icon
            name={saving ? "loader" : "check"}
            size={15}
            className={saving ? "set-spin" : undefined}
          />
          {saving ? "保存中…" : "保存"}
        </button>
      </footer>
    </form>
  );
}
